# Changelog

All notable changes to Toolport are documented here. Format loosely follows
[Keep a Changelog](https://keepachangelog.com/); versions match the GitHub releases.
Entries before the rename below shipped under the project's former name, Conduit.

## [Unreleased]

### Security

- **A process that bound the approval broker's endpoint after the app had gone could
  approve gated calls in its place, and be handed the arguments first.** The gateway
  dialed whatever `approval-endpoint.json` named and believed whatever came back. The
  descriptor survives a crash or a force-kill, and nothing authenticated the peer that
  answered: the literal bytes `"approved"` were a complete decision. Because the
  request is written before the reply is read, such a peer also received the call's
  real arguments, including the rehydrated values behind a PII release. The gateway now
  opens every dial with a random challenge that the broker must answer with an
  HMAC-SHA256 proof of the shared token, and sends nothing until the proof checks out;
  a peer that cannot read the owner-only descriptor cannot produce it, so it sees no
  request and its answer is never read. The failure is reported as `unreachable`, so a
  restarted app is still found on the re-read, and it is still fail-closed. On Unix the
  broker also listens on a socket file in a `0700` directory under the data dir, which
  a current gateway prefers, so such a peer cannot even connect on that path; the
  loopback listener stays (and is all there is on Windows), and the challenge protects
  both the same way. A gateway from before this change still reads only the loopback
  address, still reaches the broker, and is still answered. (SBS-867)

  What this does not claim: a process running as the same user as Toolport can read
  the descriptor and `registry.json` alike, and can switch human approval off directly;
  that is a sandboxing question (SBS-185), not an authentication one.

## [1.16.0] - 2026-08-22

### Fixed

- **Linux: the AppImage opens a grey empty window on Mesa (`EGL_BAD_PARAMETER`).**
  This was diagnosed in 1.15.0 as the bundled Ubuntu 22.04 WebKitGTK being too
  old for a current Mesa, and a native Arch package was added to route around it.
  That diagnosis was wrong. The bundled WebKitGTK is 2.50.4 and is not involved.
  The AppImage was bundling **wayland 1.20**, and since `AppRun` puts the bundle
  on `LD_LIBRARY_PATH` the loader applied it to the _host's_ GPU drivers too -
  which are deliberately not bundled - so `libEGL_mesa.so.0` failed to load with
  `undefined symbol: wl_fixes_interface` (added in wayland 1.23), `eglGetDisplay`
  returned nothing, and `WebKitWebProcess` aborted at launch. It presented as an
  AMD-only bug purely because NVIDIA's proprietary EGL does not link
  `libwayland-client`; every Mesa driver hit it, on X11 as well as Wayland.
  `scripts/patch-appimage.sh` now unbundles `libwayland-*` so the host's copies
  are used, and fails the release if any survive the repack. The AppImage works
  on Mesa and NVIDIA alike, so `scripts/install.sh` installs it on Arch instead
  of reaching for an AUR helper. `toolport-bin` is still published and still a
  good choice if you want a real package; it is no longer a workaround.

- **Linux: spawned processes inherited the AppImage's bundled library paths.**
  `AppRun` exports `LD_LIBRARY_PATH`, `GTK_PATH`, `GIO_EXTRA_MODULES`,
  `PYTHONHOME` and friends into Toolport, and every child inherited them. Right
  for our own bundled payload, poison for anything else: a _system_ binary loaded
  Ubuntu 22.04's glib or brotli instead of the host's and died at dynamic-link
  time on a rolling release, before running any of its own code. In practice
  clicking **Authenticate** on an OAuth server said "opening browser" and nothing
  happened, because the browser `xdg-open` launched exited with
  `undefined symbol: BrotliDecoderAttachDictionary`. The same hazard applied to
  any stdio MCP server that is a native binary or pulls a native node/python
  module. Anything that is not our own payload is now spawned with those
  variables removed (new `hostenv` module); `XDG_DATA_DIRS` keeps its host
  entries so `xdg-open` can still find `.desktop` files, and nothing else in the
  environment is touched, so a server's own `env` block still wins. A no-op
  outside an AppImage, so the `.deb`, the AUR package and dev builds are
  unaffected.

- **Windows: launch at login showed "Off" when it was on.** The status came from
  the autostart plugin's timestamp heuristic; it now reads the `HKCU` `Run` entry
  plus the state byte in `Explorer\StartupApproved\Run`, so an enabled entry stops
  reporting as disabled after a restart. A registry value it cannot make sense of
  now reports as unreadable rather than quietly as off. (#830)

- **The HTTP/OpenAPI endpoint turned itself off on every restart.** Enablement and
  port now persist in the registry, and the bearer token is reused from the OS
  keychain instead of being rotated on each launch, so a client configured against
  the endpoint keeps working across restarts. Start, stop and restore are
  serialized behind a lifecycle mutex, and a persistence failure now stops a
  newly started endpoint rather than leaving it running unrecorded. (#829)

- **OpenCode: an `opencode.jsonc` config was ignored.** Toolport always targeted
  `opencode.json`, so a JSONC config was neither read nor written. Reads, writes,
  gateway install and launch migration now share one path picker that prefers an
  existing `.jsonc` and preserves its comments and trailing commas. If both files
  exist, Toolport refuses to guess and says so instead of silently picking one. (#827)

- **Adding a server whose secret failed to save could create a duplicate.** A
  secret-write failure after a successful add is now treated as a partial success:
  every write is attempted, the failed keys are named, and the dialog stays open in
  edit mode, so retrying updates the server you just added instead of creating a
  second one. (#826, thanks @rohankumardubey)

### Changed

- **External links no longer go through `tauri-plugin-opener`.** The plugin
  spawns with our inherited environment and offers no hook to change it, so under
  an AppImage every link in the UI hit the bug above. They now go through an
  `open_external` command that reuses the same sanitised spawn as the OAuth
  flow. The `http`/`https`-only and link-local/metadata guards are unchanged and
  are now enforced on the IPC boundary as well as in the renderer.

### Thanks

One of the patches in this release came from outside.

- **[rohankumardubey](https://github.com/rohankumardubey)** - partial server adds
  no longer strand you with a duplicate on retry: failed secret writes are
  reported and the dialog stays in edit mode so the retry updates the existing
  server (#826).

## [1.15.0] - 2026-08-20

### Security

- **Release job no longer inherits Azure Trusted Signing credentials during
  frontend install.** They are now step-scoped to the Windows tauri build,
  matching TAURI and APPLE. (SBS-925)

- **Downstream stderr drain no longer grows without bound on a newline-less write.**
  Stdout was already capped at 16 MiB per line; stderr still used unbounded
  `read_line` and only trimmed the kept tail afterwards. A hostile or buggy
  stdio server that wrote a multi-GB chunk with no newline could OOM the
  gateway and take every HTTP-bridge client with it. The drain now uses the
  same `take(MAX_RESPONSE_BYTES)` bound as stdout and stops on an unterminated
  full-cap line. (SBS-930)

### Added

- **Arch Linux package: `toolport-bin`** (`paru -S toolport-bin`, or
  `omarchy pkg aur add toolport-bin`, once AUR account registration reopens
  upstream; until then `scripts/render-aur.sh <version> ./aur && cd aur &&
makepkg -si` builds the identical package with no AUR account). The AppImage
  bundles Ubuntu 22.04's
  `libwebkit2gtk-4.1`, which has no `WebKitGPUProcess` and cannot initialise EGL
  against a current Mesa, so on a rolling release the window opens grey and empty
  while `WebKitWebProcess` aborts every launch. No `WEBKIT_*` variable avoids it.
  The AUR package repackages the official `.deb` payload against the host
  WebKitGTK, the same thing the `.deb` already does on Debian/Ubuntu. Published
  by a new `aur.yml` workflow that build-tests the PKGBUILD in an Arch container
  before pushing. `scripts/install.sh` now routes Arch users there. The fat
  AppImage is unchanged and stays correct on Ubuntu/Debian.

- **Agent activity: see what your agents do outside Toolport.** Toolport routes every
  MCP call, so it sees those; it has never seen what Claude Code does natively, which
  is most of what an agent actually does (`Bash`, `Edit`, `Read`, `WebFetch`). A new
  Agent activity tab installs a small recorder into Claude Code's own lifecycle and
  keeps one line per event: which tool, in which folder, in which session. Off until
  you turn it on, and it removes itself from every file it wrote when you turn it off.

  Two limits it is built around rather than promises about it. It **cannot stop your
  agent**: the recorder is deliberately not attached to the step that can refuse a tool
  call, so no defect in it can block your work. And it **does not read your work**:
  commands, file contents and tool output are dropped before anything is written, and a
  row keeps only names, a folder, a session, and a fingerprint that cannot be turned
  back into the input.

  Every Claude Code profile on the machine is covered, not just the one Toolport
  resolved: `CLAUDE_CONFIG_DIR` picks a profile per shell, so `~/.claude` and
  `~/.claude-work` are both real and a recorder in only one of them would quietly
  under-report. As with agent rules, your file is never rewritten wholesale: only a
  marked block is added, comments and formatting elsewhere survive, and a preview shows
  the exact bytes before the first write. (SBS-822)

- **Devin Desktop, Devin Local, and Devin CLI support.** The legacy `windsurf`
  integration now uses the current Devin Desktop (Cascade) name and brand while
  keeping its compatible `~/.codeium/windsurf` paths. A separate Devin Local / CLI
  client manages the shared user MCP config and global `AGENTS.md` used by the new
  default local agent and the terminal CLI.

- **Agent rules: write your instructions once, apply them everywhere.** A new Agent
  rules tab holds one or more named rule sets and writes the active one into every
  AI client's own global rules location (`AGENTS.md`, `GEMINI.md`, `.goosehints`,
  Devin Desktop's `global_rules.md`, and a `toolport-rules.md` in the rules directory of
  clients that read one), so keeping every supported client in agreement no longer
  means hand-editing each file. Your own content is never overwritten:
  Toolport either owns its own file in the client's rules directory or owns a
  marked block inside a shared file and leaves every other byte exactly as it is.
  Each client is off until you turn it on, a per-client preview shows the exact
  bytes before the first write, and turning a client off or deleting the set
  removes what Toolport wrote and nothing else. Cursor and Warp keep their globals
  in their own UI, so the tab names them as clients it cannot write rather than
  silently skipping them. Needs no MCP server or gateway. Team instructions are unaffected and
  coexist in the same files. See [docs/agent-rules.md](docs/agent-rules.md).

- **Official brand marks for 14 more clients.** Grok Build, OpenCode, Qwen Code, Kimi
  Code, JetBrains Junie, Kilo Code, GitHub Copilot CLI, Amp, Pi, Oh My Pi, Factory Droid,
  BoltAI, AnythingLLM and Continue now show their own logo in the Clients view instead of
  a letter badge, so 32 of the 35 supported clients now carry their own mark. Crush, Jan
  and Witsy keep the badge, since none of them publish a usable vector mark.

### Removed

- **Toolport Studio is no longer a supported client.** The project is discontinued, so
  detection, the Connect flow, and its `~/.toolport-studio/mcp.json` target are gone,
  along with the session-scoped restart wording it was the only user of. Toolport now
  auto-detects 35 clients. If you had connected it, the gateway entry in that file is
  left where it is and can be deleted by hand; nothing else reads it.

### Fixed

- **Stopping the HTTP bridge no longer reports a false success.** When the child
  process survived the stop, the app cleared the handle, port and bearer token
  anyway, so the bridge read as stopped while it was still listening and nothing
  was left to retry with. The stop failure is reported, and the state is kept
  when the child is still running so a later stop can finish the job.
  (#736, thanks @YuukiRitoTeng)

- **The gateway no longer speaks on stdout before the client has handshaked.**
  MCP forbids a server sending anything before the client's
  `notifications/initialized`, and the gateway builds its catalog on a
  background thread that announces the result whenever it finishes. A client
  that spawns the process early and sends `initialize` seconds later (Grok Code
  does exactly this) read `notifications/tools/list_changed` as the FIRST frame
  of the stream, rejected it, and then looked like it had simply never been
  answered. It cost 80 of 128 sessions their handshake for most of a day. The
  notification is now withheld and replayed once the peer has both spoken past
  `initialize` and been answered at least once, so the first frame a client
  reads is always a reply to something it asked. Withheld, not dropped: the
  catalog really did change while the client was starting up. (SBS-1019)
- **A connection test whose details changed underneath it no longer reports a
  verdict for the old details.** Editing a server's command or URL while its
  test was in flight left the finished result on screen as though it described
  what is now in the form. The in-flight test stays visibly busy until it
  settles and its result is then discarded, and a superseded test can no longer
  overwrite a newer one. (#739, thanks @forever-ivy)
- **Catalog and Onboarding tell a failed stack fetch apart from an empty one.**
  A `listStacks()` failure rendered as "no stacks", indistinguishable from
  having none, with no way to retry. Both surfaces now show a skeleton while
  loading and an inline "Try again" on failure, and the stack region renders
  independently of the popular-catalog empty state, so an empty catalog cannot
  hide working stacks. (#732, thanks @rohankumardubey)

- **The Linux AppImage no longer forces X11 in a way nothing can override.**
  `linuxdeploy-plugin-gtk` writes `export GDK_BACKEND=x11` into an AppRun hook
  that is sourced AFTER the caller's environment, so `GDK_BACKEND=wayland` was
  silently ignored ("Trying x11 backend", then a tao panic). On Ubuntu/Debian
  that is only opinionated; on a Wayland session whose Xwayland cannot survive
  the app (reproduced on Arch/Hyprland in a VMware guest on `vmwgfx`) it was
  fatal and nearly undiagnosable: the first launch printed `EGL_BAD_PARAMETER`,
  showed no window, and killed Xwayland **session-wide**, breaking `xdg-open`
  for every other app; every launch after that produced no window, no output and
  no error, because the process connected to the orphaned X socket and blocked
  forever. Release builds now rewrite that line to
  `export GDK_BACKEND="${GDK_BACKEND:-x11}"` - the same default, so nothing
  changes for anyone who sets nothing - and the release fails if the line the
  patch expects has disappeared.

- **The GHCR gateway image rebuilds from the current commit.** `docker-publish` was still
  caching all of `src-tauri/target` keyed only on Cargo.lock, the same shape that shipped
  v0.3.12/v0.3.13 with a stale signed binary. It now uses the same Swatinem/rust-cache
  eviction as `release.yml`, so workspace crates always recompile while third-party deps
  stay cached. (SBS-926)

- **HITL and routine audit rows no longer count as successful tool calls.**
  Prometheus `toolport_tool_calls_total`, Activity "calls logged", and team
  showback `calls` treated every `audit.jsonl` line as a routed call and a
  missing `ok` as success. An approved human-approval gate writes a
  `kind:approval` decision (`ok:true` on purpose, so a deny stays out of the
  error rate) plus the timed exec, so one approval showed as two successful
  calls. Advisor / suggestion / candidate lines omit `ok` and were counted as
  successes while the Activity list painted them as failures. Aggregators now
  require `ok` to be present and skip `kind` in {approval, routine, advisor,
  suggestion, candidate}. (SBS-932)

- **Homebrew cask snapshot was three releases behind.** `packaging/homebrew/toolport.rb`
  still said 1.11.0 after 1.14.0 shipped, and `docs/RELEASING.md` had no tap-bump
  step, so the next release would leave `brew install --cask tsouth89/toolport/toolport`
  stale again. The snapshot now matches 1.14.0 (sha256s from the published dmgs)
  and the release doc names `tsouth89/homebrew-toolport`. The live tap is a
  separate repo; bumping it is that step, not this file. (SBS-936)

- **A refused Team Instructions rewrite no longer deletes the last-good org rules.**
  `apply_instructions_to` only recorded a target when `write_target` returned
  `Applied`. Error, TooLong (a Devin Desktop char-cap miss) and BlockedOverride then
  hit `remove_recorded`, which stripped the working v1 block, persisted the new
  content watermark, and left later syncs with nothing to retry — coverage of
  the missing file against too-long v2 is `TooLong`, not `Stale`. Last-good now
  stays on disk and in the recorded set when a rewrite is refused; a real
  removal (org cleared, client gone, path moved) still cleans up. (SBS-917)

- **Kimi Shared HTTP Connect writes `url`, not Qwen's `httpUrl`.** Connect, rescope
  and reset for Kimi went through the generic JSON editor, which remapped remotes
  via Qwen's `url` → `httpUrl` whenever the map key was not VS Code `"servers"`.
  Kimi requires `url` and rejects `httpUrl`. The Kimi writer already emitted the
  right shape; Shared HTTP now uses that same formatter. (SBS-921)

- **Agent rules Preview no longer looks like a dead button.** The preview card renders
  after the clients list, so on any window too short to reach it, clicking Preview
  scrolled nothing and showed nothing: the card was open the whole time, below the fold.
  Opening a preview now brings the card into view, and its header names the client, which
  the path alone does not settle wherever two clients share one file (Claude Code / VS
  Code, Gemini CLI / Antigravity). Deliberately still an inline card rather than a dialog:
  a modal makes the page behind it inert, and this card is a live panel that clears itself
  when the editor or the view moves under it. The scroll is not animated for anyone whose
  system asks for reduced motion: `index.css` already zeroes `scroll-behavior`, but an
  explicit `behavior` in the options dict beats that CSS property, so the component reads
  the preference itself.

- **`docs/agent-rules.md` now names every client with no rules file, not two of them.**
  The "no rules file Toolport can write" section named only Cursor and Warp, but the
  Clients section builds that list from whatever is detected, so a user with LM Studio,
  Jan, Hermes, Claude Desktop or Continue installed saw names the doc never mentioned.
  All seven are now listed with the reason each one is on it, including the note that
  Claude Desktop there is the chat app - Claude Code inside the desktop app shares
  `~/.claude` and is already covered by the Claude Code row. Continue's stale
  "deferred" comment in `clients.rs` is corrected too: its `.continue/rules/` is
  per-project and its user-level rules are a `rules:` array inside
  `~/.continue/config.yaml`, which fits neither of Toolport's strategies, and
  `continuedev/continue` was archived read-only in June 2026 besides. Continue stays a
  detected MCP client. Docs and a comment only; no behaviour change.

### Internal

- **Dead `McpSession::upstream_call` shim removed.** Nothing called it.
  (#805, thanks @forever-ivy)

- **CI apt installs are bounded and retried instead of hanging.** `apt-get
update` on the hosted runners intermittently stops responding rather than
  failing, which burned three jobs' entire timeouts on one pull request without
  ever reaching a compiler, and a run stuck in progress also blocks
  `gh run rerun --failed`. `scripts/ci-apt-install.sh` now bounds each attempt,
  retries, gives apt a short enough acquire timeout to fail over to its backup
  mirrors on its own, downloads before installing so a throttled mirror is
  interrupted rather than crept through, and installs from cache with
  `--no-download` so the final step has no network left to stall on.

- **Every CI job now has a timeout, and apt retries instead of hanging.** `Rust Clippy`
  and `Linux build + test` carried no `timeout-minutes`, so they inherited GitHub's
  6-hour default; the jobs in `audit`, `docker-publish`, `release` and `winget` had none
  either. A flaky Ubuntu mirror made that concrete on one pull request, stalling
  `apt-get update` in three separate jobs without ever reaching a compiler. An unbounded
  hang is the worst shape available: it burns the whole budget, and a run stuck in
  progress also blocks `gh run rerun --failed` on the jobs that genuinely failed, so
  recovery needs a manual cancel. All 14 jobs across the 6 workflows now carry an
  explicit backstop, sized per job and generous where a tight limit could kill a real
  release. The five `apt-get update` call sites are now one script,
  `scripts/ci-apt-install.sh`, which bounds each attempt and retries, so a bad mirror
  costs seconds rather than a job.

- **The headless security smoke no longer fails on a stderr race.** `expectBindRefusal`
  read the gateway's captured stderr as soon as the process emitted `exit`, but Node fires
  that while piped stdio can still hold undelivered bytes. The Windows runner duly reported
  `refusing to bind 0.0.0.0` without the `without HTTP authentication` tail the assertion
  matches on, failing a run in which the gateway had behaved correctly - and the identical
  assertion passed on the next host, which is the signature of a race, not a defect. The
  capture is now read only after stderr closes, bounded so a stream that never closes
  cannot wedge a security check.

- **A Windows write now retries instead of losing the race.** `atomic_write` published its temp
  file with a single `rename`. On Windows that fails outright while any other handle holds the
  destination, and something usually does - Defender, the search indexer, a backup agent - for
  a few milliseconds. `hooks::tests::preview_text_is_the_bytes_install_actually_writes` duly
  failed one Windows run with `install_at` returning an error while the identical test passed
  everywhere else. This is not only a test problem: users writing `settings.json` on a machine
  with antivirus hit the same edge. The publish rename now retries with a capped backoff, bounded
  at 8 attempts so a genuinely locked destination still reports its error. Unix is untouched,
  where `rename(2)` is atomic against open handles and a failure is real.

- **Three more sources of intermittent test failure, found by auditing rather than waiting.**
  The Windows Job Object test waited for its pid file to _exist_ and then read it once, but the
  launcher creates the file and fills it in separate operations, so a read in between parses an
  empty string and panics - the Unix sibling had been fixed for exactly this and the fix was
  never mirrored. `LockTimeoutOverride` saved and restored the lock-timeout variable per guard,
  so with several suites holding it at once the first one out reverted it while the others were
  still relying on it, dropping them to the 5s production default mid-test; it is now refcounted.
  And the HTTP concurrency test asserted an absolute 400ms wall-clock budget after two
  guess-sleeps, which a loaded runner can miss while behaving correctly; it now waits for a real
  signal that the slow call has parked and asserts the property it means - that the fast response
  came back while the slow one was still in flight.

### Thanks

Four of the patches in this release came from outside.

- **[forever-ivy](https://github.com/forever-ivy)** - the stale connection-test
  verdict, so a test whose connection details changed underneath it stops
  reporting on the old ones (#814), and the removal of a dead
  `McpSession::upstream_call` shim (#805).
- **[YuukiRitoTeng](https://github.com/YuukiRitoTeng)** - stopping the HTTP bridge
  now reports a failure instead of a false success, and keeps the child handle so
  a later stop can retry (#788).
- **[rohankumardubey](https://github.com/rohankumardubey)** - stack loading
  failures surfaced in the catalog and in onboarding, instead of a failed fetch
  rendering as "no stacks" with no way to retry (#817).

## [1.14.0] - 2026-08-16

Agents can now keep what worked. A proven multi-tool orchestration can become a
saved routine that survives sessions and clients, with a human approving the exact
definition every time one is persisted.

The rest of the release is mostly a security pass, and the credential one is the
reason to upgrade rather than wait: the redaction gate in front of the public share
link, the diagnostics bundle and the team config push missed several of the most
ordinary ways a key is spelled, so live tokens could ride out to a public URL. Two
more findings in the same family are closed here, along with a set of local-file
permissions that were wider than intended and an installer that never checked who
signed the build it was about to install.

A run of hardening across the app, on one theme: a check that could not finish used
to look exactly like a check that passed. A reload signal, a vault read, a restart
check, a backup stat each had a failure path that came back looking like good news.
They now report the failure, so what the app shows you is what it actually knows.

### Added

- **Persistent agent routines.** A proven multi-tool Code Mode orchestration can be
  promoted into a saved, parameterized routine that outlives the session and works
  from any client. Promotion is the only way in: `toolport_run_script` gains an
  immutable input mode (inputs schema-validated, deep-frozen in the VM, dropped after
  assessment), only immutable runs are promotion-eligible, and `toolport_save_routine`
  takes a `runId` rather than source, so free-typed source can never be persisted.
  Every save raises a one-shot desktop approval card showing the business summary,
  the calls, the dependencies, the risk class and the content hash, with no
  session-wide or always-allow shortcut. Saved routines are advertised as first-class
  tools and preflight their arguments against the stored schema and observed
  dependency fingerprints, failing closed to code mode on drift. A routine advisor
  watches for repeated same-shape calls and puts strong candidates in a passive
  Suggested routines queue in Settings, rather than prompting the model in-band.
  Writes are off until you turn them on. (#625)
- **Short install commands.** `irm https://toolport.app/install.ps1 | iex` on Windows and
  `curl -fsSL https://toolport.app/install.sh | bash` on macOS and Linux, replacing the
  86-character raw.githubusercontent URLs. Both redirect to a pinned commit rather than
  a branch: these are piped into a shell, so the content behind them should not be able
  to change without a reviewed change on both sides.
- **winget package.** `winget install Toolport.Toolport` on Windows once the manifest is
  published, and each release submits its own update.
- **The audit log records which client made the call.** Records carry the caller's
  name alongside the server and tool, so an audit answers "who invoked this?" rather
  than only "what ran?". (#722)
- **Re-approve every drifted tool at once.** A lost or unreadable pin baseline used to
  leave the catalog hidden with no way back except approving each tool by hand.
  The baseline is recoverable, and a bulk **Re-approve all** clears a whole drift set
  in one deliberate action. (#747)

### Security

- **Credentials no longer ride out through share links, diagnostics or the team push.**
  One redaction gate sits in front of three egress paths: the public share link, the
  diagnostics bundle people paste into issues, and the team config push that reaches
  the org control plane and every teammate. It missed the ordinary spellings:
  `--api-key=sk-...`, `--header=Authorization: Bearer ...`, `X-API-Key:`, `Cookie:`,
  and the split form `--token sk-...`, whose value sits in its own argument with
  nothing on it to recognise. Redaction now reads the whole argument list rather than
  one argument at a time, so the split form is caught, and the flag name stays visible
  while its value goes. A shared setup is meant to be readable, not to carry your keys.
  (SBS-889)
- **A server that echoes your API key no longer writes it into the audit log.** The
  audited error text is the defended body, but "defended" is not "secret-free": the
  injection scan looks for instruction overrides and the PII pass matches PII shapes,
  and an API key is neither. A server answering `invalid api key sk-live-...` put a
  live credential into `audit.jsonl` and into every CSV exported from it. No attacker
  needed, just a server that echoes its input on failure. A credential pass now runs
  over the text before it is stored, and it reads JSON bodies as well as prose while
  leaving an ordinary failure message readable. (SBS-890)
- **Local logs are owner-only from the start.** Ten append-mode logs were created with
  the process umask, which under the usual 022 means world-readable, and only became
  owner-only at their first size-triggered rotation. `oauth-debug.log` never rotates
  and `inspect.jsonl` is not touched while capture is off, so those two never got
  there at all. This matters most for `gateway.log`, which records the local approval
  broker's bound port: on a shared machine a second OS account could read it. Every
  log is now created 0600 and an existing wider file is tightened on next write, so an
  upgrade fixes the files you already have. No-op on Windows. (SBS-868)
- **The macOS installer checks who signed the build.** `codesign --verify` only proves
  a bundle satisfies its own embedded requirement, so an artifact tampered with before
  upload and re-signed with any other Developer ID would have passed. The installer now
  requires Toolport's team identifier and fails closed otherwise. It also stages the
  new app and swaps by rename, where it used to delete the installed app before
  copying: an interrupted install could leave a machine with no Toolport at all.
  (SBS-897)
- **The installers' cross-OS hints stop routing around the pinned-commit rule.**
  `toolport.app/install.sh` and `/install.ps1` redirect to a pinned commit precisely
  because they are piped into a shell, and both scripts then told a user on the other
  OS to fetch the unpinned `main` copy. Both hints use the pinned URLs now, and CI
  fails on any reference to a movable ref. (SBS-894)
- **A downstream server cannot forge Toolport's own error envelope.** A private field
  the gateway uses to report protocol errors was honoured on results coming back from
  a server, which let a hostile server author an error attributed to Toolport and skip
  the whole content-defense layer for that result. It is now stripped at the transport
  boundary, before anything reads it. (SBS-891)
- **A revoked token can no longer stay live.** The generation bump that tells a running
  gateway to reload swallowed a failed registry write, so a revoke could remove the
  credential from the keychain, never reach the gateway, and still report success. The
  failure is now propagated and the partial outcome is stated plainly. (#737)
- **A locked or unreadable keyring is no longer read as "no secret stored".** A failed
  vault read used to be indistinguishable from an absent secret, so Toolport could mint
  a replacement bearer, report a missing refresh token, or show a stale secret status.
  Each of those paths now distinguishes the error from the absence. (SBS-840, SBS-841,
  #758)
- **Disconnect fails loudly when the shared HTTP bearer cannot be revoked.** Reporting
  a client disconnected while its token still works is the one outcome that matters
  here. (SBS-845)
- **The npx spawn guard covers the eval flags it missed, and Teams stops following
  redirects with a member bearer.** A redirect would have replayed
  `Authorization: Bearer` to a host of the redirector's choosing; team servers under
  review are now an execution gate rather than a label. (#711, #713)

### Fixed

- **A space no longer smuggles a forged Toolport marker past the voice rewrite.**
  The gateway-voice matcher folded zero-width, bidi, fullwidth and homoglyph
  evasions but compared whitespace literally, so `[ Toolport advisor: …]` with a
  plain space (or a tab, a newline, a no-break space, or a doubled space inside
  the brand) reached the model unchanged while the zero-width form was caught.
  Whitespace is now folded like the other evasions: ignored before the brand and
  collapsed to one space inside it. (SBS-896)
- **Team Instructions now follow `XDG_CONFIG_HOME` for Goose and Zed.** Connect
  already wrote those clients' MCP configs under XDG (#757 / SBS-847); the rules
  writer still hardcoded `~/.config`, so an org push could succeed and never
  apply. Absolute `GOOSE_PATH_ROOT` is honoured on both the config and rules
  paths (`<root>/config/config.yaml` and `<root>/config/.goosehints`). A member
  who upgrades in place also gets the block moved to the new path: an unchanged
  org text used to skip the write entirely, so the new location stayed empty and
  coverage reported Stale until someone edited the instructions. (SBS-899)
- **Untrusted MCP output can no longer speak as Toolport.** The external-data
  wrapper is Toolport-branded (it still said `conduit` after the rebrand) and
  sanitizes the server label, so a quote in a resource URI cannot close the
  marker. Downstream text that imitates `[Toolport advisor:]`, `[Toolport shaped]`,
  `[Toolport:]`, or `[conduit:]` is rewritten before it reaches the model,
  including tools/list, search (ranked and pinned results), tool and resource
  and prompt output, and resource errors, even when content defense is off. The
  rewrite covers a whole tool definition (title, annotations, parameter
  descriptions, enum values, property names) and a whole result (structured
  output, prompt messages in any shape), and it folds the same zero-width,
  fullwidth, and homoglyph evasions the injection scanner already folds.
  (SBS-896)
- **A registry that was not really read no longer counts as "no HTTP clients".**
  `--insecure-loopback` treats an empty `http_clients` list as permission to bind and
  serve without a bearer, and that list came back empty for three unrelated reasons: a
  load error falling back to `Registry::default()`, a load that could not read the file
  at all (locked, permissions) and reported it as absent, and a load recovered from a
  backup, which is the state before the last save and so is missing exactly the save
  that registered the first client. Loading now reports where the registry came from,
  and the open branch requires a load that actually saw the configured state. The check
  is also live rather than decided at boot: the file watcher republishes that verdict
  with every reload, so a registry corrupted while the gateway runs closes the listener
  instead of quietly re-opening it. A missing registry file is a real first run and still
  opens with `--insecure-loopback`, and a failed load with `TOOLPORT_HTTP_TOKEN` still
  binds. (SBS-900)
- **A transient missing quarantine store no longer installs an empty block set.**
  Rewriting `quarantine.json` uses a temp file plus rename, so a reader can see
  `NotFound` for a tick. That used to look like "nothing is blocked" and the
  gateway would un-hide every quarantined tool. The read now retries the same
  way the pin store already does, and a miss after those retries is an error
  unless this profile has never written pins (a real first run). A rebuild
  keeps the live set, or hides the catalog on a cold start until the store
  reads. (SBS-871)
- **Codex Connect honors `CODEX_HOME`.** Connect, migrate, and launch-time
  re-point wrote `~/.codex/config.toml` even when Codex was reading
  `$CODEX_HOME/config.toml`, so a relocated live config never got a gateway
  entry and Toolport still reported success. Team-instructions `AGENTS.md`
  follows the same home. Empty or relative `CODEX_HOME` still falls back to
  `~/.codex` (Toolport's cwd is not Codex's home). The same class is now
  honored for Gemini CLI (`GEMINI_CLI_HOME` →
  `$GEMINI_CLI_HOME/.gemini/settings.json` and `GEMINI.md`), Grok Build
  (`GROK_HOME`), and Qwen Code (`QWEN_HOME`). A leading `~` in `QWEN_HOME` is
  expanded, because Qwen expands it too; the other three use their env value
  verbatim, so a literal `~` there stays a fallback. A GUI-only Toolport
  process still cannot see a shell-only export; that limitation already
  applied to `CLAUDE_CONFIG_DIR`. (SBS-885)
- **A flagged tool result can no longer self-close its provenance wrap.** A
  payload that embedded `[/conduit: end external data]` (or `[/Toolport`) used
  to terminate the wrap and leave a forged `[Toolport: …]` line reading as
  gateway voice. The close tag is now per-call and includes a random nonce,
  and an embedded close marker is stripped to a plain note that cannot read as
  a terminator. The match runs on the folded form, so a close hidden with
  zero-width characters, a Cyrillic homoglyph or fullwidth brackets is caught
  too. (SBS-892)
- **A Personal-scoped HTTP bearer can no longer call a team server whose id
  collides after sanitizing.** A personal server named Team Slack becomes
  `team-slack`; a team server named slack becomes `team_slack`. The HTTP bridge
  used `sanitize_segment` as the profile allow-set key, and both ids map to
  `team_slack`, so the Personal token could list and call the team Slack server.
  Scope now keys on the raw registry id (SBS-866). Tool names are still sanitized.
  The bridge answers its first `tools/list` and OpenAPI document from the on-disk
  catalog before any downstream server has connected, so scoping in that window
  resolves the owning server through the registry and withholds any tool whose
  prefix two server ids share, instead of guessing from the prefix.
- **Unreadable activity/security log no longer renders as empty healthy.** A failed
  read of `audit.jsonl`, `security.jsonl`, `inspect.jsonl`, or `search-trace.jsonl`
  used to come back as an empty list, so Activity showed **No tool calls yet** /
  **Protection active**. A missing file is still empty (honest). Any other IO error
  now rejects the invoke (including the dashboard's `audit_stats`), and the existing
  error/retry UI surfaces it. `GET /metrics` answers 500 on the same unreadable log
  instead of 200 with every series missing, so a Prometheus scrape fails rather than
  reading as an idle instance. Security events also no longer lose an older row when
  a corrupt or mid-write line lands in the newest page. (SBS-873)
- **Windows keyring and install.ps1 tests can now block merge.** Branch
  protection still requires only the check named `Build + test`. That name now
  belongs to a merge-gate job that fails unless the Linux suite, the
  cross-platform headless Rust matrix (including the Windows keyring tests),
  and the `install.ps1` Pester job all succeeded. Adding those names as
  separate required contexts still needs a GitHub settings click; this change
  does not edit branch protection.

- **A write-named tool cannot disarm drift quarantine with `destructiveHint: false`.**
  MCP annotations are untrusted unless the server is. Drift severity now escalates
  when the tool name itself claims write capability (`delete`, `run`, …), even if
  the server set `destructiveHint: false` at first sight. Call-time confirm still
  honours an explicit false hint. First sight of that contradiction is recorded
  in Activity and is not quarantined. (SBS-875)
- **Connect and launch-time repoint no longer strip comments from Codex/Grok
  `config.toml` or comments and YAML anchors from Goose/Hermes/Continue
  `config.yaml`.** Those writers used to parse with `toml` / `serde_yaml` and
  pretty-print the whole file, so a Connect click or `repoint_stale_gateways`
  dropped every `#` comment and expanded every `&anchor`. Unrelated keys
  survived as data; the annotations did not. TOML now goes through `toml_edit`
  DocumentMut and YAML rewrites only the `mcp_servers` / `extensions` /
  `mcpServers` node, matching the JSONC CST contract. A user who Connects or
  disconnects now keeps the comments and anchors they had outside that node.
  (SBS-884)
- **Failed routine-suggestion load no longer hides the save queue.** Settings treated
  an unreachable `list_routine_suggestions` as "nothing pending" and hid the section,
  including wiping cards already on screen when a later refresh failed. A failed load
  is now its own state: empty after a successful read still hides; error shows retry
  and keeps the last loaded queue. (SBS-879)
- **A diagnostics bundle or second gateway can no longer see an empty `gateway.log` or lose a connect-failure line.** Trim used to rewrite the shared log in place, so a concurrent diagnostics read could land in the empty window and a concurrent append could be overwritten by the pre-truncate snapshot. Trim now replaces the file atomically under the same lock as append.
- **SECURITY.md now matches the HTTP bind admission policy.** Every bind, loopback
  or not, needs HTTP authentication: a bearer token, or at least one registered
  HTTP client in `registry.json`. A hand-launched loopback gateway with neither
  does not bind; it exits 1 unless `--insecure-loopback` is passed, and that flag
  opens an unauthenticated listener only while no token is set and no HTTP clients
  are registered. SECURITY.md also now explains what a registered HTTP client is
  and how the desktop app creates one. (SBS-878)
- **Connect keeps stow/chezmoi config symlinks.** Writing a client config
  (Connect, Disconnect, migrate, launch-time re-point) used to replace a
  symlink at the config path with a regular file, leaving the file in the
  dotfiles repo unchanged. The next `chezmoi apply` / `stow` restored the
  old content and the gateway entry disappeared. Writes now follow the
  link and update the target, so the path under home stays a symlink.
  (SBS-886)

- **Linux `.deb` installs a `toolport` command.** The package still ships the
  crate binary as `conduit` (compat alias) and now also puts `toolport` on
  `PATH`, matching the AppImage installer and the brand. `install.sh` tells apt
  users to run `toolport`. (SBS-846)
- **A second Claude Code profile no longer gets stuck on an old gateway.**
  `CLAUDE_CONFIG_DIR` is usually set per shell or per launcher rather than exported, so a
  machine often has several Claude configs (a personal `~/.claude` beside a work
  `~/.claude-work`). Toolport resolved exactly one of them, and every other one it had
  written kept pinning whichever gateway binary was current the day it was written.
  Because pruning deliberately keeps recent binaries, that profile went on launching
  superseded gateway code indefinitely and the reaper could not win: it stopped the
  process and the config Toolport never updated started it again. Every Claude config is
  now re-pointed on launch, and each one's gateway binary counts as referenced so pruning
  cannot delete it. Strictly a repair: a profile with no Toolport entry does not get one,
  and a hand-customized entry is still left alone.
- **A failed old-gateway check no longer hides the warning.** Settings swallowed the
  error, so "no apps need a restart" and "we could not find out" looked identical. The
  failure is now shown with a Retry that re-runs the check in place, and running
  **Stop old gateways** clears it, since that result answers the same question. (#730)
- **A failed backup stat aborts the config write.** The pre-write backup is what makes
  a client-config write recoverable; continuing without one turned a safety step into
  a silent no-op. (#745)
- **A transient registry read failure retries instead of showing an empty app.**
  A reload that lost a race with a write used to leave the UI looking like a machine
  with nothing configured. (#724)
- **A failed quarantine poll is no longer reported as a confirmed zero.** "Nothing is
  quarantined" and "we could not ask" are different answers, and only one of them is
  reassuring. (#741)
- **Launch at login stays disabled until the OS state is actually known.** The switch
  used to render as a verified Off while the real value was still unread. (#721)
- **The gateway routes every tool it advertises.** The collapse guard could keep a tool
  in the catalog whose route had been dropped, so a model could call something that
  then failed to resolve. (#717)
- **`install.sh` verifies its downloads like `install.ps1` already did.** Per-asset
  digest, published size, https-only URLs, and the AppImage now stages in a temp
  directory so a failed verification cannot delete a working install. (#744)
- **Linux fixes.** AppImage launch-at-login points at `$APPIMAGE` rather than the
  FUSE mount that disappears between runs; the bundled gateway is recopied when its
  contents change rather than only its size; `XDG_CONFIG_HOME` is honoured for Zed,
  Goose and AnythingLLM; a crash in gnome-keyring under concurrent Secret Service
  sessions is avoided by serializing them and retrying a dead daemon; and blocking
  file and keychain reads no longer run on the GTK main loop, which is what made the
  window controls stop responding. (SBS-844, SBS-843, SBS-847, SBS-815, SBS-813)
- **Concurrent OAuth sign-ins agree on the outcome.** A lock drop that failed to record
  its verdict let a waiting process treat an unfinished attempt as a completed one.
  (SBS-842)
- **The audit log survives concurrent appends and rotation.** Append and rotate are
  serialized, so a rotation cannot drop a record another process just wrote. (#708)
- **Opening the data folder reports its failure.** It used to fail silently, leaving
  the button looking broken rather than the action. (#768)
- **Hermes is detected at its Windows platform config path.** (#746)
- **A rich confirmation dialog renders valid markup.** Descriptions containing block
  elements produced invalid DOM nesting. (#723)
- **The onboarding dialog is named for screen readers**, and the role choice exposes
  its selected state. (#726)

### Internal

- **Clippy runs in CI.** (#749)
- **Tests stopped depending on the machine that runs them.** Four launcher tests read
  the developer's real npx cache, and the SSRF tests assumed `.invalid` never resolves,
  which is not true behind a resolver that answers every name. Both now inject what
  they were reading from the host, so a clean checkout is green. (SBS-839, SBS-827)
- Three multi-process contention tests no longer flake on a loaded runner. (SBS-895)

### Thanks

A big cycle for contributions: fifteen of the patches below came from outside, including
the routines feature that leads this release.

- **[forever-ivy](https://github.com/forever-ivy)** - persistent agent routines, the
  headline feature of this release: immutable runs, human-approved promotion, first-class
  routine tools and the Suggested routines queue (#706). Also the registry-reload retry
  (#724), and the report that our SSRF tests fail behind a resolver that answers every
  name, which turned out to be right and is fixed here (SBS-827).
- **[aryansk](https://github.com/aryansk)** - eight patches: download verification in
  `install.sh` (#744), the failed-reload-signal fix that stops a revoked token staying
  live (#743), the quarantine poll that no longer reads as a confirmed zero (#742), the
  aborted write on a failed backup stat (#745), routing every advertised tool (#717),
  launch-at-login state (#721), valid dialog markup (#723), and onboarding
  accessibility (#726).
- **[joyheroes](https://github.com/joyheroes)** - surfaced two silently swallowed
  failures: the old-gateway restart check (#778) and opening the data folder (#768).
- **[BharadwajKanneveti](https://github.com/BharadwajKanneveti)** - the caller name in
  audit records, so an audit says who invoked a tool (#722).
- **[Vermitrude](https://github.com/Vermitrude)** - the Clippy gate in CI (#749).
- **[rohankumardubey](https://github.com/rohankumardubey)** - serialized audit append
  and rotation (#708).

If we missed you, open an issue.

## [1.13.0] - 2026-08-13

Toolport installs two new ways: as an agent plugin any conformant client can pick up,
and on Windows through a one-line command instead of a trip to the Releases page.
Pseudonymization gains the piece it was missing, a way for a human to release one
value to one server, so the workflow it used to dead-end now has an answer.

Most of the rest of this release is one defect wearing different faces: code that
read a failed probe as good news. A failed audit baseline, health check, security
read, or integrity load could each come back looking like "all clear" and let the
app act on it. Each one now fails closed.

### Added

- **Agent plugin.** Toolport now ships as an [Agent Plugins 1.0](https://agent-plugins.org)
  package (`toolport-agent-plugin.zip` on each release): one install connects VS Code,
  GitHub Copilot CLI, the Copilot app, and other conformant clients (plus Claude Code,
  via the bundled dual layout) to the local gateway, with a skill teaching the agent
  the search → call workflow. The plugin launches the gateway the desktop app already
  installed, so plugin installs share your existing servers, credentials, and profiles.
  If the app already manages that client, disconnect it there first or the gateway
  connects twice.
- **Windows one-line install.**
  `irm https://toolport.app/install.ps1 | iex`,
  matching the macOS and Linux one-liners. It resolves the release through the GitHub
  API, picks the NSIS asset for the machine's architecture, and refuses to install
  anything it cannot verify against the per-asset digest (`-AllowUnverified` is the
  explicit override). A signature that disagrees with the file is fatal; a missing one
  is a warning, since builds before Azure signing shipped unsigned. (#610)
- **Release one pseudonymized value to one server.** Scoping rehydration to the minting
  server closed a cross-server exfiltration channel but left a legitimate workflow with
  no path at all: read a customer from a CRM, mail them through a different server, and
  the call simply failed. Refusal is still the default, with a human decision as the
  remedy. The prompt names the destination, each token, and its real value, and grants
  that one value to that one server. There is deliberately no blanket per-server grant,
  an "always allow" on a tool can never release a later value, and a headless gateway
  with nobody to ask refuses exactly as before. Audited by hashing the arguments, so
  released values never reach the log. (SBS-696)
- **Pseudonymization counts in the audit log, CSV export, and Activity.** This path
  fails open: a full session map or an over-cap result leaves values in the clear, and
  a call where redaction quietly did not apply used to be indistinguishable from a clean
  one. `piiReplaced` is absent when redaction was off and present-and-zero when it ran
  and matched nothing, and `piiIncomplete` is written only when true, so the fail-open
  case is greppable. Activity shows "N pseudonymized" with a warning badge when the pass
  did not fully apply. Counts only; values never leave the session map. CSV columns are
  appended at the end, so positional consumers keep working. (SBS-607)
- **`toolport.checkpoint()` in code mode.** A last-write-wins resume marker a script
  sets itself, alongside the automatic call ledger. Unlike `progress`, which is
  positional, this holds author-chosen state (`{ lastInsertedId: row.id }`). Costs
  nothing against `max_calls`, capped at 4096 bytes, and surfaces in the failure text
  and in `structuredContent.toolportScript.checkpoint`. (#663)
- **Server-declared tool icons** render in the tool browser. `data:` sources only: a
  remote icon URL is not a picture but a request the app makes to a server-chosen host
  on every paint, reporting when Toolport is open and from what IP with no tool call
  involved. (SEP-973)
- **URL-mode elicitation**, so a server that needs you to finish something in a browser
  works even with a client that does not support it: Toolport brokers the prompt in the
  desktop app instead of letting the call fail. A server-provided link is a phishing and
  internal-network primitive, so only credential-free HTTPS URLs resolving exclusively to
  public addresses are allowed, under the same host boundary as Toolport's SSRF defenses,
  and the origin shown to you is derived from the parsed URL rather than from anything the
  server says about itself. (SBS-707)

### Fixed

- **A failed read no longer looks like success.** Onboarding needs a successful audit
  baseline before anything counts and stays in checking or unavailable until an
  authoritative health result exists, with concurrent probes sharing one promise instead
  of returning an empty list. Activity keeps the last known security events across a
  failed refresh and never shows Protection active without an authoritative read. A
  rejected client-secret probe stays unknown instead of reporting "no secret stored",
  which was blocking scope edits for secrets that were already vaulted. Integrity-store
  failures fail closed, and interrupted pin updates recover. (SBS-718, SBS-719, SBS-720,
  SBS-721, SBS-722, #697)
- **The HTTP bridge validates `Origin`, not just `Sec-Fetch-Site`.** Under DNS rebinding
  the attacker's domain resolves to loopback, so the browser calls it same-origin and the
  old guard waved it through, worst under `--insecure-open` where there is no token.
  Absent `Origin` stays allowed, so curl and Open WebUI's backend are unaffected, and a
  deliberately exposed bridge can still serve its own browser UI by matching the bound
  address or listing hosts in `TOOLPORT_HTTP_ALLOWED_ORIGINS`. No name resolution is
  involved, so a domain that merely resolves to the bridge stays foreign. (SBS-452)
- **Folder scoping keeps working as Roots is deprecated.** Roots was the only input
  folder-scoped auto-routing ever had, so a client that never sent it, or stopped
  mid-session, silently dropped to the unscoped profile and reached more servers than
  intended. The project root now resolves from `TOOLPORT_ROOT`, then the client's roots
  for the whole deprecation window, then the gateway's working directory. (SEP-2577)
- **Stale gateway and catalog state can no longer become permanent.** `CLAUDE_CONFIG_DIR`
  is honored instead of hardcoding `~/.claude.json`. A downstream that answers
  `tools/list` with a truncated catalog no longer becomes cached truth for every gateway
  started afterwards. A republish under a content-addressed filename no longer flips a
  managed client to Customized, which had pinned it to a superseded gateway permanently
  (npx, docker, and wrapper scripts are still Customized). A confirmed rebuild collapse
  can now land instead of being held forever. (#698)
- **Profiles with similar names no longer share state.** Per-profile stores were named by
  a lossy slug of the display name, so "Work Prod" and "Work/Prod" shared cache, pins,
  and quarantine. Files are keyed by a hashed profile id now, unambiguous legacy files
  migrate, and a slug collision fails closed rather than merging two profiles. (SBS-715)
- **A Windows credential being replaced no longer reads as missing.** Windows reports the
  base credential absent for the instant a replace is in flight, which surfaced as a
  spurious "not authenticated" during OAuth refresh, since the app writes these and the
  gateway reads them from another process. Absence now only counts once it survives
  bounded retries with a real backoff. A mitigation rather than a guarantee: measured
  over 9,600 racing reads, 37 torn reads became 4. (SBS-711)
- **Atlassian OAuth on Windows**, plus a longer refresh-lock wait so a slow refresh is
  not abandoned. (#685, SBS-705)
- **Cancellation is honored end to end.** The HTTP transport aborts or forwards
  cancellation and retry backoff observes it, so abandoned work releases its per-server
  slot. The updater refuses to install while gateways are still running and recovers the
  HTTP bridge if an install fails after shutdown. (SBS-716, SBS-717)
- **PII session maps are cleared when sessions end.** (SBS-704)
- **Requests with no transport fail closed** instead of proceeding. (SBS-551)
- **Tray and update lifecycle**, including approval requests being delivered once rather
  than repeatedly. (SBS-146)

### Thanks

Two patches this cycle, one of them a code-mode feature listed above:

- **[Vermitrude](https://github.com/Vermitrude)** - `toolport.checkpoint()`, the
  script-declared resume marker a code-mode script sets alongside the automatic call
  ledger (#689).
- **[rohankumardubey](https://github.com/rohankumardubey)** - ran the headless Rust
  suite across macOS, Linux and Windows in CI, where the no-desktop build had only been
  exercised on one platform (#671).

If we missed you, open an issue.

## [1.12.0] - 2026-08-09

Two features ship for the first time. PII pseudonymization replaces personal data in
tool results with tokens before the model sees them, restoring the real values only
for the server that provided them. OAuth client credentials let a headless server —
one nobody can click a browser sign-in for — get a real token. Both are off or opt-in
by default.

Several things that were only safe within one process are now safe across processes:
rate-limit counters, OAuth token refresh, and the pseudonym map. Each client spawns
its own gateway, so one process was never the real shape.

**Upgrading a Teams deployment:** the instructions receipt hash changes once in this
release. See Changed below before rolling it out.

### Added

- **PII pseudonymization.** Emails, phone numbers, card numbers, IBANs, IP addresses
  and provider-shaped API keys become stable tokens (`⟦EMAIL_1⟧`) before the model
  sees them, and are restored on the way out. The mapping stays in memory. Off by
  default, and a reduction rather than a guarantee: a value no detector recognizes
  passes through, as does everything once the per-session cap is reached. (SBS-346)
- **OAuth client credentials for headless servers.** Discovers the endpoint,
  negotiates the auth method, and reacquires before expiry. Never falls back to a
  browser flow, which would be unusable where this is needed. (SBS-524)
- **Old gateway binaries are cleaned up** instead of accumulating (~18 MB a release).
  Keeps anything running, named by a client config, known to be relaunching, or
  recent enough to still be cached. (SOU-484)
- **Apps still launching an obsolete gateway are named.** An app caches its spawn
  command at startup, so stopping the process is not enough. Settings and a launch
  notification list which apps need restarting, each entry clears itself, and Settings
  now reports processes it could not stop. (SOU-435)
- **Keyboard shortcuts.** `Ctrl/Cmd+1`–`6` switch view, `/` or `Ctrl/Cmd+F` focuses
  search, `Ctrl/Cmd+N` adds a server, `Ctrl/Cmd+R` refreshes, `?` lists them.
  (SBS-143)
- **The window reopens where you left it** instead of resetting to a fixed centered
  geometry on every launch. (SBS-144)
- **`npx` servers start about four times fewer processes.** Toolport resolves the
  entry point directly instead of the `cmd.exe` → `npx` → shim → server chain, which
  came to roughly 423 processes for 72 servers on one machine. Version ranges and
  `@latest` still go through `npx`. Because this skips the install step a server stays
  on the cached version; `TOOLPORT_NO_DIRECT_SPAWN=1` restores the old path. (SOU-550)
- **Modern MCP extensions.** Opaque client extension settings pass through to
  downstream servers and compatible declarations aggregate in `server/discover`;
  `io.modelcontextprotocol/tasks` handles stay bound to the server that created them
  and survive router rebuilds; clients can detect Toolport via `app.toolport/gateway`,
  which reports discovery, code-mode, agent-control and approval state. (SOU-453)
- **MCP Apps.** Catalog fetches negotiate the HTML MIME type, Apps hosts get UI-linked
  tools even under lazy or grouped discovery, and `ui://` resources route through
  their owning tool. App HTML stays byte-faithful for host CSP, and app-only tools
  stay out of model-facing search. (SOU-453)
- **Code mode** can validate a script without running it, and a script that fails
  partway reports the calls it already made instead of discarding them. (SBS-646,
  SBS-647)
- The gateway binary supports `--help` and `--version`, and rejects unknown flags.

### Changed

- **The team-instructions receipt hash now uses SHA-256** instead of Rust's default
  hasher, whose algorithm carries no guarantee across compiler releases. **This
  changes every member's reported hash once.** The Teams coverage dashboard will show
  one round of drift that is not real drift; it reconverges as members report in on
  this version. Same width and format as before. (SBS-460)

### Security

- **A pseudonym only resolves for the server that produced it.** Tool results are
  attacker-controlled, so an injected result could otherwise talk the model into
  putting a CRM's token into a URL for an unrelated fetch tool, sending the real value
  to whoever wrote the injection. Calls carrying another server's token are refused.
  The cost is deliberate: passing a record between servers no longer works unattended.
  (SBS-605)
- **The pseudonym map is cleared when a conversation ends** and on a fresh handshake,
  on every transport. It previously lived for the whole gateway process with no
  eviction. (SBS-605)
- Retry fields on a resumed request get the same pseudonym handling as the arguments
  beside them, so a host answering a prompt from model context no longer relays a
  literal token downstream. (SBS-606)
- **OAuth token refresh is serialized across the app and the gateway.** They share one
  keychain and could spend the same refresh token twice, which a provider with reuse
  detection answers by revoking the whole family. (SBS-479)
- **Rate-limit counters are safe across concurrent gateways.** Each client spawns its
  own and they overwrote one another's counts, so org caps under-counted and did not
  hold. Overlapping caps sharing a window no longer double-count. (SBS-680, SBS-609)
- **An empty quarantine store fails closed.** A truncated file silently re-exposed
  every tool held after high-risk drift or baseline tamper. (SBS-654)
- A cleartext-auth refusal no longer echoes URL-embedded credentials into the error,
  where they reached the activity view and logs. (SBS-636)
- **OAuth hardening** (SOU-451): authorization responses bind to the validated issuer,
  and stored credentials refuse to cross a changed issuer; discovery checks
  path-specific protected-resource metadata before the origin fallback and rejects
  metadata for a different resource; Bearer `WWW-Authenticate` challenges are honored
  and validated through the existing TLS and SSRF guards; runtime `insufficient_scope`
  challenges preserve prior scopes and open a bounded consent step-up; servers
  advertising Client ID Metadata Documents use Toolport's stable HTTPS identity, with
  Dynamic Client Registration as the fallback.

### Fixed

- Reading a resource whose URI contains non-ASCII characters no longer fails. The
  template matcher walked the URI by byte, so backtracking through a multi-byte
  character crashed the router. (SBS-620)
- A code-mode script's return value is no longer silently corrupted: a `Date` came
  back as `{}` and a `BigInt` as `null`, both reported as success. Values that cannot
  be represented now error instead. (SBS-631)
- Connect, rescope, disconnect and migrate leave a restart reminder in the panel
  rather than only a toast that fades, with wording matched to the action. (SBS-336)
- The "Stop old gateways" panel no longer reads as though it contradicts itself. It
  could report nothing running directly above a list of apps still launching one —
  both true, since a client spawns the gateway on its next tool call. It now says so,
  and each row shows the process id so near-identical entries can be told apart.
- The Activity list no longer comes up short when the audit log holds an unreadable
  line, which used to consume a slot in the page instead of being skipped. (SBS-677)
- Servers launched through `npx.bat` keep their package identity on import and paste;
  only `.exe`, `.cmd` and `.ps1` were recognized as package runners. (SBS-664)
- A registry change saved in the app could briefly revert on screen when the disk
  watcher read the file outside the lock guarding the in-memory copy. (SOU-329)
- OAuth loopback callbacks reject an empty authorization code instead of showing a
  success page and sending an invalid token exchange.
- The onboarding "What is Toolport for Teams?" link opens the explainer page rather
  than the app sign-in, the right destination before you have a team. (SBS-461)

### Maintenance

- `cargo clippy --all-targets` is error-free. A deny-by-default `never_loop` was the
  last hard failure blocking a clippy gate in CI. (SBS-434)
- Code mode's real limits are documented and pinned by tests: a script that never
  calls a tool is bounded by iteration and recursion counts, not the wall clock. Both
  fail closed. (SBS-430)
- Tests pin that pseudonyms resolve as the last step before dispatch, an ordering that
  had regressed once without anything catching it. (SBS-614)
- Team URLs come from one place, CodeRev reviews once per PR, and text files check out
  with LF endings on every platform. (SBS-461)

### Thanks

Ten people sent patches this cycle, including two new clients, a fix that stops
installs stripping comments out of hand-edited configs, and the CI job that now guards
the gateway's security suite:

- **[Vermitrude](https://github.com/Vermitrude)** - Factory Droid CLI support, and
  `--help` / `--version` on the gateway binary, which previously accepted unknown
  flags silently (#579, #627).
- **[slegarraga](https://github.com/slegarraga)** - removed a dead data-directory
  shim and corrected three pieces of CONTRIBUTING that had drifted from the code
  (#638, #639, #642).
- **[rohankumardubey](https://github.com/rohankumardubey)** - put the headless gateway
  security smoke suite into CI, where it had never run, and exposed sidebar and toggle
  state to screen readers (#580, #621).
- **[syf2211](https://github.com/syf2211)** - turned raw stack traces into readable
  headlines for DNS, TLS, 429 and 5xx connection failures, and made catalog search
  include the category so searching a visible heading actually matches (#611, #614).
- **[alexgaribay](https://github.com/alexgaribay)** - Kimi CLI support, and a fix for
  duplicate Tauri context creation across startup paths (#658, #662).
- **[arimu1](https://github.com/arimu1)** - installing the gateway no longer strips
  comments out of JSONC client configs (Zed, VS Code, Kilo Code) (#592).
- **[adity982](https://github.com/adity982)** - Crush MCP client support (#407).
- **[georgeatparallel](https://github.com/georgeatparallel)** - Parallel Search in the
  curated catalog (#615).
- **[aryansk](https://github.com/aryansk)** - documented the gateway's public
  environment overrides (#640).
- **[AshSgDe29071999](https://github.com/AshSgDe29071999)** - documented curated stacks
  and replaced a hardcoded count that broke whenever one was added (#616).

## [1.11.0] - 2026-08-01

Toolport's Streamable HTTP endpoint now speaks the modern MCP transport while
keeping the legacy initialize/session flow on the same URL. This release also
adds four clients, hardens registry recovery, and fixes a collection of routing,
search, and import edge cases.

### Added

**MCP 2026-07-28 over Streamable HTTP.** Modern clients can use `POST /mcp`
without `initialize` or `Mcp-Session-Id`; every request carries its own protocol
metadata and the required `MCP-Protocol-Version`, `Mcp-Method`, and optional
`Mcp-Name` headers. Existing clients on the legacy initialize/session flow keep
working on the same endpoint. (#584, #586)

- **Multi-round-trip requests and stateless HITL.** Modern tool calls return
  `resultType`, surface approval as `input_required`, and resume from opaque
  `requestState` plus `inputResponses`. A denied call never reaches the server;
  an accepted retry executes the bound call once. (#585)
- **Modern subscriptions.** `subscriptions/listen` replaces the legacy GET
  stream for 2026-07-28 clients, filters notifications per listener, tags them
  with the subscription id, and flushes events promptly. (#583, #590)
- **Modern downstream HTTP headers.** Toolport sends per-request protocol and
  routing headers to modern HTTP servers, including schema-declared
  `x-mcp-header` values, while legacy downstream servers keep their existing
  wire format. (#584)
- **Cacheable, deterministic catalog results.** Modern list/discovery results
  carry `ttlMs` and `cacheScope`, and tools, prompts, resources, and templates
  use stable ordering so caches do not churn between equivalent requests.
  (#587)
- **Four more clients.** Kilo Code, Amp, GitHub Copilot CLI, and JetBrains Junie
  are detected and configured, taking the supported client count to 31. Kilo
  Code reuses the OpenCode `mcp` shape; Amp keeps its literal dotted
  `amp.mcpServers` key and honours `AMP_SETTINGS_FILE`. (#538, #553, #576, #578)

### Security

- **Registry recovery fails safe.** A corrupt primary registry is restored only
  from a valid backup; Toolport no longer silently starts from an empty registry
  that drops configured policy. (#582)
- **Corrupt integrity pins fail closed.** A damaged pin store can no longer make
  an existing tool definition look trusted. (#581)
- **Removing a server clears its security state.** Tool overrides, pins,
  per-server result budgets, injection-block exemptions, and fingerprint-bound
  approvals are removed with the server, so a later server reusing the id cannot
  inherit stale policy. (#509)
- Import-time private-host detection now matches the OAuth SSRF guard's full
  private-address ranges. (#564)

### Fixed

- Unsupported legacy `initialize` versions now return the supported versions so
  clients can negotiate instead of failing ambiguously. (#588)
- **A share link survives a failed copy.** Creating a link and then failing to
  copy it reported the whole operation as failed, and where the Clipboard API is
  unavailable the failure was thrown synchronously, so a link that had been
  created fine was surfaced as `Couldn't create a link`. The link now stays
  visible and only the copy is reported as failed. (#560)
- Empty search states are clearer, unusable MCP commands are reported directly,
  keyword parameters no longer trigger placeholder false positives, and
  cross-server names no longer create false rate-limit matches. (#567, #568,
  #572, #577)
- Inspect skips corrupt lines before applying its result limit, tiny savings
  percentages remain visible, token counts are rounded before unit selection,
  and search-limit errors use the configured constants. (#569-#571, #573)
- Remote Crush entries are no longer misclassified as OpenCode during import,
  and the import-review copy now matches the behavior it describes. (#541,
  #566)

### Maintenance

- Shared cost estimation and civil-date conversion replace duplicate local
  implementations, and verified dead client/desktop types were removed.
  (#574, #575, #589)

### Thanks

Patches this cycle came from:

- **[BharadwajKanneveti](https://github.com/BharadwajKanneveti)** - server-state
  cleanup, routing/search fixes, inspect recovery, and shared helper extraction
  (#509, #567, #572-#575).
- **[ColumbusLabs](https://github.com/ColumbusLabs)** - GitHub Copilot CLI and
  JetBrains Junie support plus search, reporting, and display fixes
  (#568-#571, #576-#578).
- **[wenn-id](https://github.com/wenn-id)** - Amp support (#538).
- **[rohankumardubey](https://github.com/rohankumardubey)** - Kilo Code support
  (#553).
- **[Vam-si-krish](https://github.com/Vam-si-krish)** - preserving share links
  when clipboard copy fails (#560).
- **[arimu1](https://github.com/arimu1)** - aligning import-time private-host
  detection with the OAuth SSRF guard (#564).
- **[Vermitrude](https://github.com/Vermitrude)** - Crush/OpenCode import
  disambiguation and dead-code cleanup (#541, #589).

If we missed you, open an issue.

## [1.10.0] - 2026-07-29

Toolport speaks the current MCP revision, stale gateways actually stop after an
upgrade, approvals stay bound to what you approved, and a batch of transport and
code-mode hardening.

### Added

**Toolport speaks MCP 2026-07-28 over stdio, in both directions.** A client on the
new revision can talk to Toolport, and Toolport can talk to a server on it, with
every existing client and server continuing to see byte-identical traffic. Both eras
run on the same stdio endpoint and are detected per connection, so there is nothing
to migrate and nothing to configure.

Over Streamable HTTP, Toolport stays on the established revision for now. A modern
client receives exactly the response the spec defines as the fall-back signal, so it
negotiates down cleanly instead of failing. That half arrives with
`subscriptions/listen`.

Three things now work through the gateway that could not before:

- **Progress notifications reach your client.** A server reporting progress during a
  long call has it relayed back, routed to the client that asked for it.
- **Large results keep their full envelope.** Shaping an oversized result preserves
  `_meta` and any fields Toolport does not recognise, so nothing a server sends is
  dropped in transit.
- **Structured error codes survive the hop**, so a client can act on a
  machine-readable code rather than parsing a message string.

### Security

**An approved tool call is re-checked against the live gateway before it runs.** A
human approval was validated against a snapshot taken before the hold, so a tool that
was quarantined, released, or had its definition changed during the approval window
still executed against the pre-hold view. Approvals now rebind to the live router and
fail closed if the definition fingerprint moved or the tool is blocked, with a clearer
"this approval is stale" message. (SOU-321, SOU-322)

**Vendor auth hints require an exact domain match.** Lookalike apex domains
(`clerkauth.com`, `evilgithub.com`) could inherit a real vendor's auth hints and token
URL through prefix/suffix matching on the second-level label. Matching is now exact,
`api.githubcopilot.com` gets its own entry, and a trailing-dot FQDN still resolves.

### Fixed

**Old gateway processes are stopped after an upgrade, on every OS.** Upgrading left
older versioned gateways (`toolport-gateway-1.9.4.exe` and friends) running, so
security and policy fixes in the new binary never took effect for clients still talking
to them. Identity is now path-based across Windows, macOS, and Linux. On macOS the
process listing used an argv that Apple's `ps` rejects, so it saw zero gateways; on
Linux a binary replaced in place is now correctly treated as obsolete rather than
protected. Settings gains a **Stop old gateways** action. (SOU-414)

Note the limit: an AI client caches the gateway command when **it** starts, so whether
stopping the old process is enough depends on the path that got cached. Where the binary
is replaced in place, the same path already resolves to the new one and the next spawn
picks it up. Where the path is one an upgrade never rewrites, it does not: on Windows the
filename carries its version (so an upgrade never has to overwrite a locked file), and on
any OS an app can still be pinned to an install location you have since moved away from.
In those cases, restart the client app itself. Clients started after the upgrade are
unaffected.

**The Shared HTTP bridge comes back after the reaper stops it.** Reaping a bridge whose
binary was replaced left HTTP and OpenAPI clients with nothing listening until someone
reopened Settings.

**A Continue Shared HTTP bearer now reaches the wire.** The token was written under
`env`, which Continue does not forward for remote servers, leaving a plaintext bearer
on disk that never authenticated. It now goes under `requestOptions.headers`, matching
Continue's contract. Ownership re-detection reads both.

**Client config backups no longer accumulate live bearer tokens.** Every config write
copied the previous file and nothing ever pruned them, so a Shared HTTP client's
backups piled up carrying working credentials. Capped at five generations per file,
matching the registry.

**Resource subscriptions clean up when a session is replaced**, and a subscriber
waiting on another client's open no longer gives up while that open is still
succeeding.

**Code mode budget and isolation.** `fetchResult` shares the call and wall-clock budget
rather than paging without limit, async workers reinstall the active session for host
calls, and a corrupt registry no longer boots with code mode enabled.

### Also in this release

- Pasting a Crush config no longer fails as a malformed OpenCode one. Both use a
  top-level `mcp` key, so the shape of `command` decides which it is. (#497)
- Rate-limit counters stay in memory until a data directory is bound, instead of
  writing a stray counter file into the working directory. (#543)
- The share-link copy button confirms it copied, and says so when it could not.
  (#549)
- Coverage for the import-review shell and private-host classifiers, and for
  gateway filtering during client migration. (#547, #510)
- `fmtMs` and `fmtDollars` moved into `lib/utils` with tests. (#548)
- Error strings, the benchmark write-up, the security notes, and CONTRIBUTING all
  say what the code actually does. (#539, #545, #546, #540)

### Thanks

Patches this cycle came from:

- **[AnayGarodia](https://github.com/AnayGarodia)** - benchmark and security docs, the
  share-link copy fix, `fmtMs`/`fmtDollars` extraction, and tests for the
  import-review classifiers (#545, #546, #547, #548, #549).
- **[Vermitrude](https://github.com/Vermitrude)** - OpenCode/Crush paste
  disambiguation (#497).
- **[snowyukitty](https://github.com/snowyukitty)** - keeping unbound rate-limit
  counters in memory (#543).
- **[rohankumardubey](https://github.com/rohankumardubey)** - test coverage for
  gateway filtering during client migration (#510).
- **[cyforkk](https://github.com/cyforkk)** - normalised the error strings (#539).
- **[HaimiyaWasn](https://github.com/HaimiyaWasn)** - CONTRIBUTING correction (#540).

If we missed you, open an issue.

## [1.9.6] - 2026-07-27

Client config ownership, Shared HTTP connect, code mode v2 (parallel + typed stubs),
native resource subscriptions, gateway hardening, and safer vendor matching.

### Discovery

**Code mode on by default.** `toolport_run_script` is advertised unless you turn **Code
mode** off in Settings (or set `"codeMode": false` in the registry). Each in-script call
still hits the same scope and approval gates as `toolport_call_tool`. Code mode is not a
security boundary (agent-supplied JS). `TOOLPORT_CODE_MODE=1` still force-enables.
Existing registries that already store `"codeMode": false` stay off. (SOU-397)

**Code mode parallel calls and typed stubs.** Scripts get `callAsync` / `Promise.all`
with bounded host parallelism, scoped `servers.*` typed stubs, full intermediate
results and `fetchResult` handoff. (#480–#483 / SOU-348)

### Added

**Per-client transport: Spawn (stdio) or Shared HTTP.** Integrations can connect a
client to the supervised HTTP bridge instead of spawning its own gateway. Native
remote shapes (VS Code, OpenCode, Qwen, Hermes, Continue) get a url + bearer entry;
clients that only support stdio (Claude Desktop, etc.) get an opt-in `npx mcp-remote`
bridge. Tokens are vaulted; ownership records never store bearers. (SOU-407)

**Native MCP resource subscriptions.** Subscribe/unsubscribe and `resources/updated`
fanout (with producer verification), resource templates + completions, paginated
catalogs preserved. (#474–#479, #484)

### Fixed

**Client gateway ownership is now a first-class state (Managed / Customized /
Absent).** Toolport records what it last wrote into each client's config and surfaces
hand-edited entries as "custom configuration" in Integrations, with an explicit Reset
to default (confirm before overwrite). Launch re-point and Connect no longer silently
clobber a customized entry. Pre-ownership installs still use the command-basename
heuristic. (SOU-406, follow-up to #487)

**A hand-edited gateway entry is no longer reverted on every app launch.** The
launch-time re-point recognized its own entry by _name_, so an entry still called
`toolport` but pointed at something else - an `mcp-remote` bridge against the HTTP
endpoint, a container, a wrapper script - was treated as a stale install and rewritten
back to the default stdio command every time the app started. Re-pointing now requires
the stored command to actually name a Toolport gateway binary; anything else is treated
as user-managed and left exactly as written (and the skip is logged). Genuine
migrations - an older version, the pre-rename `conduit-gateway`, the pre-rename data
directory, an unversioned install path - are unaffected. (#487, #488)

**A machine-wide `TOOLPORT_HTTP` / `CONDUIT_HTTP` no longer hijacks client-spawned
gateways.** HTTP mode replaces the stdio transport, so an inherited value left every
MCP client with a gateway that never answered its pipe, and every gateway after the
first colliding on the shared port (`WSAEADDRINUSE`) - which some clients treat as
fatal. The env forms are now ignored, with a warning, when stdin is a pipe. The
desktop app, the Docker images, and the documented headless setup all pass `--http`
explicitly and are unaffected; use the flag in scripts and services too. (#487)

**Vendor auth hints match on domain-label boundaries only.** Bare needles like
`clerk` / `github` no longer match attacker subdomains (`clerk.evil.com`), and full
domain needles require a real host suffix. Spoofed hosts can no longer skip the live
probe via `force_kind`. (#417, #492)

**Headless `secrets.enc` set/delete is locked** against concurrent writers. (SOU-332)

**Profile scope tool-fetch UI** shows failures, ignores stale errors after a newer
load, and scopes loading state per server. (#468)

**Search efficiency and routed-call audit overhead** improvements. (#472, #473)

## [1.9.5] - 2026-07-25

Finishes the Conduit → Toolport rename for what users and configs see, keeps
legacy `CONDUIT_*` env aliases working, and ships security, Teams policy, quarantine,
and client polish that landed after 1.9.4.

### Branding and upgrade migration

**Client configs write `toolport`, not `conduit`.** New connects and a launch
migration rename the MCP entry, move the data directory leaf
`Conduit` → `Toolport` when safe, and rewrite client env keys to
`TOOLPORT_CLIENT_ID` / `TOOLPORT_PROFILE` while still accepting the old
`CONDUIT_*` names. Downstream children no longer inherit `TOOLPORT_*` control-plane
env (vault key / HTTP token). Deep links accept `toolport://` and still open
legacy `conduit://` share links. (#445 and follow-ups)

### Security and integrity

**Opt-in block-on-injection, with org force.** Content-defense hits can refuse the
tool result instead of only flagging it; Teams can force the policy on members.
(#465)

**Structured tool results are scanned more thoroughly**, including head/tail of
large `structuredContent`, with redaction on hit. (#455)

**Corrupt quarantine store fails closed** instead of treating the file as empty
and unblocking tools. (#448)

**Upstream MCP responses are shape-validated** before use. (#452)

### Teams and gateway policy

**Org `allowedTools` maps into local profile `tool_scope`**, with allowlist id
mapping and apply receipts so admins can see policy land on the desktop.
(#457, #458, #456)

**Org rate limits enforced in the local gateway.** (#461)

**Optional per-call audit export to the org.** (#460)

**Profile `tool_scope` enforced on the HTTP bridge** as well as stdio. (#459)

### Observability and clients

**Opt-in Prometheus `/metrics`** on the gateway HTTP surface (`TOOLPORT_METRICS=1`).
(#464)

**Grok Build** (xAI terminal coding agent used with Toolport Studio) as a
first-class client: detect, one-click connect, `~/.grok/config.toml`. (#433)

**Toolport Studio** as a first-class client (`toolport-studio`): detect install
markers, one-click connect to `~/.toolport-studio/mcp.json`, profile scope, and
session-aware connect toasts. Studio still auto-discovers the gateway without
Connect; Connect pins profile and Activity attribution.

**Quarantine cards show annotation detail** and notify when new entries appear.
(#439)

**Client detection errors surface in the UI** instead of failing silently. (#466)

**Restart toast after connecting a client** so users know to reload the AI client.
(#317 / #442)

**Client scope copy matches behavior** (active profile, not “all servers”). (#447)

### Reliability and polish

**Activity rows keep expansion across the 3s live refresh.** (#450)

**Timestamps go through shared `fmtTs`.** (#451)

**Quarantine.json re-parse skipped when mtime+len unchanged.** (#435)

**Invalid discovery / HTTP / budget env values warn** instead of failing quietly.
(#453)

**CI runs Rust integration tests**; notarytool submit is time-bounded. (#454, #441)

### Docs

Headless, Open WebUI, Docker compose, README, and env reference prefer
`TOOLPORT_*` names and document `CONDUIT_*` as still-accepted aliases.

## [1.9.4] - 2026-07-22

Toolport blocks a tool when its definition changes in a risky way. This release fixes the
part where un-blocking it didn't work, and makes the whole thing visible instead of buried.
It also lands a security pass that closes several ways a malicious server could reach past
the gateway, adds four new clients, and clears a batch of reliability and correctness bugs.

### Quarantine: blocked tools you can actually see and unblock

**Re-approving a blocked tool now unblocks it.** Re-approving cleared the list in the app,
but the gateway kept refusing the call with "re-approve to restore" - telling you to do the
thing you had just done, with no way out from inside the app. Restarting Toolport or toggling
a server off and on was the only escape. The gateway now reconciles what's blocked against
what you've approved, so a re-approved tool works on the very next call. (#395)

**Blocked tools surface anywhere in the app.** A card now appears with the reason the tool
was blocked and a re-approve button right there, plus a count on Settings so it stays easy to
find. Previously the first sign of trouble was an agent call failing, and the remedy was
buried in Settings. (#401)

**A damaged quarantine file no longer un-blocks tools.** If the file recording what's blocked
couldn't be read, it was treated as "nothing is blocked", quietly dropping the protection. It
now keeps enforcing what it already knows, and says so. (#399)

### Updating

**Updating now replaces the gateway your AI clients are using.** Toolport runs a small
gateway process for each connected client, and that's where most fixes live - including the
re-approve fix above. Those clients could keep running the _old_ gateway until you restarted
them, so you'd install a fix, watch the problem persist, and reasonably conclude the update
hadn't worked. Toolport now retires out-of-date gateways when it starts, and each client picks
up the new one on its next request. (#404)

**No more black command windows flashing on launch.** A few internal housekeeping steps were
briefly opening console windows on Windows at startup. Harmless, but alarming. (#405)

### Clients

**Four new clients: Witsy, Oh My Pi, OpenCode, and Qwen Code.** Toolport now detects 26
clients. OpenCode and Qwen Code each use their own config shape - OpenCode stores a command
as an argv array, and Qwen Code distinguishes streamable-HTTP from SSE servers - and Toolport
reads and writes both correctly. (#366, #365, #411, #415)

**Number and boolean values in a server's `args` are kept** when importing or pasting a
config, instead of being silently dropped (which would shift every argument after them). (#416)

**Pasting a Continue `config.yaml` block works.** "Paste from client config" rejected
Continue's format with "Could not detect format", even though Toolport already reads and
writes that exact file for the Continue client. Environment variable values in the pasted
block are preserved. (#403)

**Downstream servers run in their own process group on macOS and Linux**, so a server
starting up can't disturb the display of a terminal-based AI client. (#364)

### Reliability

**Tool name collisions get stable suffixes.** When two tools on a server would share the same
exposed name, the loser gets a `_2` suffix - but that was assigned by list order, so a server
reordering its own tool list could swap the suffix between two real tools, and a cached tool
name would quietly start calling the wrong one. Suffixes are now assigned by tool name, so
they don't move. (#408)

**Downstream server processes are fully cleaned up on macOS and Linux.** Killing a server (a
toggle, or a catalog rebuild) killed only the wrapper, leaving `npx`/`uvx` child processes
running. Toolport now tears down the whole process group, so nothing is left behind. (#406)

**A clear error when a server's working directory doesn't exist**, instead of a confusing
spawn failure, including which configured path (and which unset variable) was the problem.
(#410)

### Security

**Tool error messages are now scanned like tool results.** Content defense labels untrusted
tool output as data so a server can't slip instructions into your agent - but it only covered
successful results. A server could bypass it by returning an _error_ whose message carried the
payload. Errors now go through the same scan and size cap. (#429)

**A server that can't be resolved is no longer treated as local.** During sign-in (OAuth), an
unresolvable server address was classified as "local" and had its network-safety checks
switched off - a path a malicious server could use to aim the sign-in flow at your own
network. A server now has to positively resolve to a private address to earn that trust.
(#430)

**Renaming a tool no longer disables its safety checks.** A tool you renamed couldn't be
blocked and wasn't watched for risky changes, so a rename silently opted it out of the
protection the rest of your tools get. Renamed tools are now blocked and watched like any
other. (#431)

**Vendor detection matches the server's host, not anywhere in its address**, so a path-based
gateway (for example `.../github`) is no longer mistaken for the vendor named in the path -
which could otherwise misjudge whether that server needs a token. (#413)

Cleared seven advisories from the dependency tree: two high-severity in `brace-expansion` and
`js-yaml`, then one high and four moderate reaching us through `fast-uri` and `hono`. The
second batch arrived because `shadcn`, a code-generation CLI, was listed as a production
dependency - moving it removed that whole subtree from the shipped app rather than patching
versions one at a time. (#367, #396, #402)

### Polish

**Activity no longer shows a red "0%"** for a server that does have errors. Small error rates
read as "0.2%" or "<0.1%" instead of rounding away to nothing. (#388)

**The new-profile name box clears when you cancel**, so reopening it no longer offers to
create a profile you had already abandoned. (#386)

### Thanks

This release includes work from:

- [@floze-the-genius](https://github.com/floze-the-genius) - OpenCode client support (#411), Qwen Code client support (#415), Continue YAML write-safety tests (#414)
- [@bradhallett](https://github.com/bradhallett) - Oh My Pi client support (#365), process-group isolation on Unix (#364)
- [@amitvijapur](https://github.com/amitvijapur) - Witsy client support (#366)
- [@BharadwajKanneveti](https://github.com/BharadwajKanneveti) - discovery-ranker blend test (#394), non-string `args` handling (#416), Activity timestamp formatting (#412)
- [@pollychen-lab](https://github.com/pollychen-lab) - Activity error-rate formatting (#388), new-profile field reset (#386)
- [@Vermitrude](https://github.com/Vermitrude) - vendor detection host matching (#413)
- [@AnayGarodia](https://github.com/AnayGarodia) - working-directory error reporting (#410), data-directory test isolation (#409)
- [@manishchalla](https://github.com/manishchalla) - Continue snippet parsing (#403)
- [@dubeyharshit0605](https://github.com/dubeyharshit0605) - ConfirmDialog test coverage (#393)

## [1.9.3] - 2026-07-18

Makes the v1.9.2 teams cost fix actually work when the app is in the tray.

### Fixed

**Team sync really does pause in the tray now.** v1.9.2 tried to pause syncing when the app
was backgrounded, but it relied on a browser signal that doesn't fire when a window is hidden
to the tray on Windows, so a tray'd app kept polling the team server and kept its database
awake. The pause now runs off the app's own window show/hide, so a connected app sitting in
the tray makes no requests at all and resumes with an immediate sync the moment you open it.
(#360)

## [1.9.2] - 2026-07-18

A cost fix for teams: an idle connected app no longer keeps the team server's database awake.

### Fixed

**Team sync pauses when the app is in the background.** A Toolport app connected to a team
long-polled the team server every ~25 seconds for as long as it was running, even minimized to
the tray with nobody using it. Each poll touched the server's database, which on a scale-to-zero
Postgres kept the compute awake around the clock. The sync loop now pauses entirely while the app
is hidden (tray/minimized) and resumes with an immediate catch-up sync the moment you bring it
back, so a backgrounded app stops hitting the server. Paired with a server-side write throttle so
even a foreground app only records presence every few minutes. (#359)

## [1.9.1] - 2026-07-16

A fast follow-up to v1.9.0: make code mode reachable, correct the activity total, and stop
the injection scanner from flagging benign shell examples.

### Added

**Turn on code mode from Settings.** Code mode (the `toolport_run_script` meta-tool) was
gated behind the `CONDUIT_CODE_MODE` env var only, so there was no way to enable it from the
app. A "Code mode" toggle now lives in Settings under Discovery, off by default. The env var
still force-enables it for power users. (#346)

### Fixed

**Activity "calls logged" shows your real total.** The summary aggregated only the last 2,000
audit entries, so the count capped at 2000 and the error rate was taken over that slice. It
now aggregates the full retained log (still bounded by the log's size cap), so the total,
error rate, and per-server breakdown are the real numbers. (#347)

**The injection scanner stops flagging benign shell examples.** A tool description that
documents a hashing pipeline (for example `... | base64 -d | shasum -a 256 | awk ...`) no
longer trips the "embedded-command" content flag. The scanner now matches a pipe into an
actual shell or interpreter (`| sh`, `base64 -d | sh`) with word boundaries, so look-alikes
like `| shasum` and decode-then-hash stay clean while real decode-then-execute still flags.
(#348)

## [1.9.0] - 2026-07-16

The orchestration-and-polish release: run whole tool sequences server-side, scope tools by
folder and by profile, and a lighter, more finished UI.

### Added

**Code mode: run a sequence of tool calls in one server-side script.** Instead of
round-tripping every call through the model, an agent can send one script that the gateway
runs in a sandboxed pure-Rust (Boa) engine, calling tools and shaping results inline. This
cuts round-trips and token overhead on multi-step work. (#335)

**Folder- and project-scoped auto-routing.** Map a workspace root to a profile so the active
server set follows the folder you're working in, with no manual switching. (#336)

**Tool-granular profiles.** A profile can now scope not just which servers are visible but
which individual tools each server exposes, so a profile can present a tight, purpose-built
tool set. (#340)

**Light and dark theme.** A System / Light / Dark control in Settings, with the light palette
tuned for real use. Existing installs stay dark; fresh installs follow the OS. (#342)

**Official client logos.** Client detail pages now show the real brand logo (Claude, Cursor,
Codex, Gemini CLI, Zed, Warp, and more), with a clean monogram fallback where no official
mark is available. (#343)

**Structured-result projection in `toolport_fetch_result`.** Large tool results can be
projected down to the fields that matter before they reach the model, instead of returning
the full payload. (#331)

### Improved

**Activity updates live while you watch it.** The feed and the Live Inspector refresh in place
as calls come in, so a running agent's activity shows up without leaving and returning to the
view. (#341)

**Clearer savings accounting.** The Activity savings banner rolls large token counts into B/T
units and frames the dollar figure honestly as a list-price upper bound before caching. (#339)

**Lazy discovery recovers from weak and zero-match searches.** The gateway detects
low-confidence lexical or hybrid rankings, preserves the ranked tools, and adds a bounded,
server-diverse fallback menu from the caller's scoped catalog. Exact searches stay compact,
and no-match responses explain how to enumerate every tool on a known server without adding
another default meta-tool. (#328)

**Transport helper text in the server dialog.** The add-server form explains the transport
options inline. (#334)

### Security

**Approvals are content-bound and typed.** A human approval is bound to the exact argument
content it was granted for, so a decision can't be reused against swapped arguments; the
approval broker connection handling is also hardened. (#332, #324)

**MCP sessions are isolated per client.** Sessions are bound to their HTTP client scope,
destructive-confirmation tokens are scoped to the client that earned them, and per-session
outbound queues are bounded. (#321, #322, #323)

**Loopback HTTP requires authentication and bounded reads.** Local gateway HTTP now requires
the bearer token and enforces request read deadlines. (#325, #326)

**Semantic embedding requests are guarded against SSRF.** Outbound embedding requests are
validated before they leave the gateway. (#320)

**Teams server updates reject stale writes.** Concurrent updates no longer overwrite newer
state. (#327)

### Fixed

**Windows stdio process trees are killed on teardown.** Spawned server process trees no longer
linger after shutdown. (#330)

**OAuth expiry is persisted and refreshed proactively.** Tokens refresh ahead of expiry
instead of failing a call first. (#329)

**Debounced config export in the Share dialog.** Rapid interactions no longer fire redundant
export work. (#338)

## [1.8.0] - 2026-07-13

The safer-control release: per-client discovery modes and broader API coverage, backed by
stronger client isolation, config integrity, and gateway resource bounds.

### Added

**Choose discovery mode per client.** Each connected AI client can now use `full`, `lazy`,
or `grouped` discovery without forcing the same mode on every other client. The choice is
stored with the client and takes effect on its next gateway start. (#309)

**Full-API catalog options for Stripe, Vercel, Cloudflare, and Clerk.** Curated overlays make
the vendors' broader official MCP surfaces available while preserving Toolport's setup and
credential guidance. (#308)

**Clear local activity data from Settings.** Audit, savings, inspection, and search-trace
history can now be removed together, with the security documentation updated to describe
the HTTP gateway and local retention accurately. (#310)

### Security

**Scoped clients no longer see metadata for renamed tools outside their profile.** Tool
ownership is resolved through the router instead of inferred from the exposed tool name,
closing a cross-client catalog leak. (#305)

**Spawned MCP servers no longer inherit Toolport control secrets.** Gateway-only environment
variables, including the file-vault key and HTTP bearer token, are stripped before every
downstream launch. (#298)

**The spawn screener closes more launcher and interpreter bypasses.** Clustered eval flags,
remote deno/bun sources, versioned interpreter names, `data:` URLs, and additional wrapper
commands are screened before execution. (#299)

**Gateway flood and oversized-input paths are bounded.** HTTP requests above the in-flight
cap receive an immediate retryable response, search queries are capped before ranking, and
stdio request frames are limited to 16 MiB without losing the next valid frame.
(#316, #317, #318)

**Release and container workflows are harder to tamper with.** Third-party actions are pinned
to immutable commits, Docker permissions are narrowed, and credentials are not persisted in
the checkout used for signed builds. (#304)

### Fixed

**Config writes stop losing valid state.** Cross-process registry mutations are serialized,
team re-sync preserves enablement across profiles, and an unparseable client config is never
silently replaced. (#303, #307)

**Client setup removes stale duplicate Toolport gateways.** Install, repair, and uninstall now
recognize legacy names and gateway commands across JSON, TOML, and YAML clients, leaving one
canonical entry while preserving unrelated servers. (#315)

**Updates reliably move clients to the new gateway binary.** Versioned and older manual
gateway processes are stopped during update so clients respawn the current build. (#289)

**Desktop failures recover instead of leaving a blank window.** A top-level error boundary
adds a reload path, and Cancel, Playground, and bulk-import flows no longer keep stale state
or perform the wrong follow-up action. (#306)

**Teams onboarding handles approval links and empty new-team configs.** Approval-gated join
links resume after sign-in, and a first config pull returning 404 is treated as an empty team
rather than a failed join. (#285, #291)

**Activity notices and tool-count copy match the current product.** Tool-less security events
receive useful labels, and discovery messaging reflects the current 4-7 meta-tool surface.
(#302, #314)

### Quality

Component-level regression coverage now protects the error boundary, bulk-import review,
server dialogs, and external-link guard alongside the existing Rust gateway suite.
(#300, #312)

### Thanks

Thanks to @sapunyangkut for improving tool-less Activity security notices (#302), and to
@tapheret2 for the external-link guard regression coverage carried into this release (#300).

## [1.7.2] - 2026-07-11

The Teams stability release: two separate freezes when joining a team, plus import,
security, and gateway improvements. (Supersedes the never-published 1.7.1 draft.)

### Fixed

**Joining a team no longer hangs the app.** The team network commands (join, background
config sync, admin push) were synchronous and ran on the app's UI thread; the sync holds a
roughly 30-second long-poll open continuously, so being connected to a team blocked the
interface and the window went "Not Responding." They now run off the main thread. (#288)

**Joining a team no longer exhausts memory.** The background sync rewrote the local registry
on a timer, and every gateway rebuilt and re-spawned each server on the change, leaking
processes until the machine ran out of RAM. A registry save is now a no-op when nothing
changed, and the gateway only rebuilds when the resolved server set actually changes.
(#286, #287)

**Bulk import handles scoped and duplicate packages.** Scoped package identity is preserved,
same-package servers with distinct names no longer collide on one id, and the import review
no longer acts on stale rows. (#280, #283, #284)

### Added

**Review bulk imports before applying.** Detect or paste a batch of servers and confirm the
full set before it lands, instead of importing blind. (#282)

**Project-root working directory for `${ROOT}` servers.** A `${ROOT}` server runs in the
client's actual project root, resolved live from its MCP roots, instead of wherever Toolport
sits. (#275)

### Security

**Tighter spawn screening.** Destructive-command screening now catches PowerShell
`-EncodedCommand` and `PERL5OPT`. (#279)

### Thanks

First-time contributors: @glasses-and-hat (unit tests for the token-savings formatter, #277)
and @SK-Sathyavada-008 (a consolidated CONDUIT environment variable reference, #278).

## [1.7.0] - 2026-07-09

This release leads with a security fix and clears a batch of rough edges around running
servers and living in the app day to day.

### Security

**Scoped clients now fail closed on a missing profile.** If a client was pinned to a
profile that later got deleted or renamed, it used to fall back to your active profile and
quietly expose that set of servers instead. It now resolves to nothing rather than the
wrong thing. Clients that aren't scoped still follow the active profile, unchanged. (#261)

### Added

**Per-server working directory.** Any stdio server can now run in a working directory you
choose, so a filesystem or grep server operates on the project you point it at instead of
wherever Toolport happens to sit. Paths support `~` and `${VAR}`; leave it blank to keep
the old behavior. (#262)

**Re-scope a client without restarting it.** Change which profile a client uses and it
takes effect on the next reload, with no client restart. The app also flags servers that
are connected but exposing zero tools, which is almost always a sign the server still
needs to authenticate. (#247)

**Copy button on server errors.** Copy a server's full error output to the clipboard in
one click. (#264)

**Dev builds keep their own data.** Development builds now store data under `Conduit-dev`,
so debugging can't touch your real setup. (#232)

**Recovery notice.** When the registry is restored from a backup, a one-time message tells
you it happened and when. (#231)

### Fixed

**One slow server no longer freezes the whole grid.** Each server is checked on its own
now, so a cold `npx` or `uvx` download stops leaving every other server stuck on
"checking" for up to a minute. (#263)

**Errors lead with the useful line.** A failed server used to bury the real problem under a
stack trace and a giant login URL. It now shows a one-line summary first, with the full
output kept below if you want it. A long OAuth URL on its own no longer looks like an auth
error. (#253)

**Servers pasted as `npx` get a real name.** Pasting a package-runner config now names the
server after the package it runs (for example `automem`) instead of `npx`, so several
`npx` servers stop colliding and hiding each other's tools. A server already saved as
`npx` needs a quick remove and re-paste to pick up the correct name. (#255)

**Registry recovery is harder to break.** Toolport now keeps a short rolling journal of
recent backups instead of a single one, so recovery isn't stuck when the latest backup is
stale. Settings written by a newer build also survive being re-saved by an older one.
(#249, #250)

**Better keyboard access.** Activity rows respond to Enter and Space, and the add-server
transport picker has a proper label for screen readers. (#264)

Also includes the OAuth callback and credential-reload fixes carried over from the 1.6.x
line. (#226, #228)

### Documentation

**Refreshed the roadmap, README, and benchmark** to match where the project actually is.
(#248)

## [1.6.2] - 2026-07-09

**Windows install fix (completes 1.6.1).** Manual installer downloads and the
1.6.0 → 1.6.1 hop now kill locked gateway processes before NSIS copies files.

### Fixed

- **Windows NSIS install with locked gateway** — `NSIS_HOOK_PREINSTALL` runs
  `taskkill` on `toolport-gateway.exe` / `conduit-gateway.exe` before file copy.
  1.6.1 only stopped gateways during in-app update from an already-updated app, so
  manual installs and upgrades from 1.6.0 still failed when Cursor held the gateway.

## [1.6.1] - 2026-07-09

**Windows auto-update fix.** If Cursor or another MCP client held `toolport-gateway.exe`
open, the in-app updater could fail to replace the install-dir binary. This patch
publishes a versioned gateway under `%APPDATA%\\Roaming\\Conduit\\bin` and stops only
spawned gateway processes before install.

### Fixed

- **Windows auto-update with locked gateway** — MCP configs point at a versioned
  `toolport-gateway-{version}.exe` under `%APPDATA%\\Roaming\\Conduit\\bin` instead of
  the install-dir copy; before updating, Toolport stops only spawned gateway processes so
  NSIS can replace locked binaries without closing Cursor or other agents. (#244)

## [1.6.0] - 2026-07-09

**Headless gateway.** Deploy `toolport-gateway` in Docker, speak MCP over HTTP/SSE, pull
a prebuilt image from GHCR. Desktop users get smoother npx/uvx first connects, AnythingLLM
support, Teams usage rollups, and registry safety fixes.

### Added

- **Headless / container gateway** — run without the desktop app: `POST /mcp`
  streamable-HTTP, env-file secrets (`CONDUIT_SECRET_KEY`), Docker +
  `docker-compose.example.yml`. See `docs/headless.md`. (#214)
- **MCP listen stream** — `GET /mcp` SSE for server→client JSON-RPC (30s keepalive when
  idle). (#216)
- **MCP server-initiated RPC passthrough (#167)** — when the upstream client declares
  `roots`, `sampling`, or `elicitation` at `initialize`, downstream servers can call
  `roots/list`, `sampling/createMessage`, and `elicitation/create`; the gateway forwards
  over stdio or HTTP MCP (inline during SSE `POST` responses). (#217, #218, #219)
- **Prebuilt gateway image on GHCR** — `ghcr.io/tsouth89/toolport-gateway:latest`
  (CI-built binary + slim runtime; ~3 min builds vs ~8 min). (#222, #223, #225)
- **AnythingLLM client** — connect from the Clients view. (#213)
- **Teams per-server usage rollups** — members report tool-call counts to the team
  dashboard (counts/estimates only; tool names stay local). (#221)

### Fixed

- **npx/uvx cold-start false errors** — download launchers (`npx -y`, `uvx`, `pnpm dlx`,
  …) get a 120s first-`initialize` budget (10s for everything else), **"Installing…"**
  UI while downloading, and background pre-warm on add. (#237)
- **SSE streaming for inline server-initiated RPC** — HTTP downstream no longer buffers
  the full body before forwarding JSON-RPC to the upstream client. (#220)
- **Registry preserved on read failure** — a corrupt or unreadable `registry.json` is
  quarantined and restored from `.bak` instead of silently reset. (#224)

### Changed

- **Gateway-only compile** — `cargo build --no-default-features --bin toolport-gateway`
  skips Tauri/WebKit for headless/CI builds; desktop default unchanged. (#225)

### Documentation

- **Headless production checklist and security guidance** — deploy checklist, inherited
  vs new security surface, and audit recommendations in `docs/headless.md`. (#242)
- **Release notes draft** — `docs/release-notes/v1.6.0.md`; updated `docs/RELEASING.md`.
- **Headless smoke tests** — `scripts/smoke-headless.ps1` (auth, MCP handshake, HITL
  fail-closed).

## [1.5.3] - 2026-07-08

Teams reliability and activation batch, ahead of the Teams launch.

### Fixed

- **Org-forced safety locks are released when you leave a team.** A team that enforced
  human-in-the-loop approval, destructive-tool blocking, content defense, or
  quarantine-on-drift used to bake that setting permanently into the member's own settings,
  so leaving the team left the lock stuck on with no way to turn it back off. These org
  forces are now tracked separately from your own settings and cleared the moment you leave;
  your own toggles are never touched. (#209)

### Added

- **"Joining a team?" onboarding path.** The first-run wizard now offers first-class
  invite-code entry, so a team member who was told to install Toolport can join their team
  immediately instead of clicking through solo setup to look for it. (#210)
- **Near-instant team policy sync.** Members now long-poll the team config, so an admin's
  policy or access change in the dashboard enforces on member machines in about a second
  instead of at the next poll interval. Falls back cleanly against an older team server. (#211)

## [1.5.2] - 2026-07-07

Post-1.5.1 security and robustness batch from a multi-dimension gateway audit
(#203 HIGH, #204 MEDIUM, #205 LOW/robustness), plus a follow-on hardening pass
(#207). All batches ship with regression tests; full suite green.

### Security

- **Approvals are now bound to the tool definition.** A "for this session" or "always" allow
  is keyed to a fingerprint of the exact tool definition it was granted for, resolved from
  the live server. If a server later changes that tool (a rug-pull), the call re-prompts
  instead of inheriting the old approval; legacy broad allows are ignored, so existing users
  re-approve once. (#207)
- **Broader destructive-tool detection.** When a server omits the MCP `destructiveHint`, the
  approval gate now also treats obvious write/delete verbs in the tool name (delete, drop,
  send, publish, truncate, upload, ...) as destructive, failing toward caution. An explicit
  `destructiveHint: false` still wins. (#207)
- **Secret redaction in shareable diagnostics.** The diagnostics summary now redacts inline
  secret arguments and credentials embedded in server URLs, and clears the live-inspection
  buffer on startup when inspection is off. (#207)
- **Spawn-guard bypass via attached inline-eval flags.** The dangerous-flag guard only
  matched interpreter flags as standalone argv tokens, so the attached form
  (`python -c<code>`, `ruby -e<code>`) from a booby-trapped server config slipped past and
  executed arbitrary code. Generalized the matcher to the attached short form. (#203)
- **OAuth DNS-rebind SSRF into the private network.** The OAuth metadata resolver refused
  only link-local / metadata IPs; RFC1918 and loopback were blocked by a separate
  pre-connect check, a resolve-then-connect TOCTOU a rebinding host could exploit. A
  stable, provenance-derived `block_private` flag now refuses private answers at connect
  time too, so a rebind can't flip it. Self-hosted LAN auth servers still work. (#204)
- **OAuth cleartext-exchange bypass.** `require_https`'s loopback exception used a string
  prefix that also matched `http://127.0.0.1.evil.com`; it now decides on the parsed host
  (`is_loopback()` / `localhost`). (#204)
- **HTTP-bridge token masked in the UI.** The bearer token that grants any local process
  access to every tool was shown in plaintext on each visit; it is now masked by default
  with a reveal toggle. (#205)

### Fixed

- **Config-wipe data loss on parse failure.** A genuinely-unparseable `codex/config.toml`,
  `~/.claude.json`, or Gemini `settings.json` was replaced with a fresh file holding only
  the gateway entry, destroying the user's model/provider/profile/MCP state. Both paths now
  fail closed and preserve the file (a timestamped backup was always taken first, so prior
  damage was recoverable). (#203)
- **Router lock held across downstream refresh I/O.** A `list_changed` from one slow
  downstream stalled every concurrent request for up to num_servers x connect-timeout; the
  refresh now runs on an off-lock router clone swapped in under a brief lock. (#204)
- **Self-heal thundering herd.** A startup burst of workers could each rebuild the router,
  spawning the full server set N times; the rebuild is now single-flighted behind a
  double-checked lock. (#205)
- **Robustness batch:** cancel-forward threads capped at 64 to stop a wedged downstream
  leaking threads; config install cap raised 8MB to 64MB for heavy Claude Code users;
  saturating arithmetic on the savings/audit hot paths; frontend polish (approval-bar
  countdown scales against the real window, stale share-export guard, capped
  dismissed-activity set). (#205)

### CI

- Pinned the release workflow's actions to commit SHAs (it holds the signing secrets);
  removed stray 0-byte local signing-key artifacts. (#205)

## [1.5.1] - 2026-07-06

A focused safety and gateway-control patch release. The headline fix is the
human-in-the-loop approval path: approval failures are now diagnosable, audited,
and resilient to stale broker descriptors instead of collapsing into a vague
timeout.

### Added

- **Grouped discovery mode.** `CONDUIT_DISCOVERY=grouped` now advertises the lazy
  meta-tools plus one `help_<server>` browse tool per connected server, giving
  weaker/local models an enumerable middle ground between the tiny lazy surface and
  the full catalog.
- **Per-registry discovery mode.** Discovery mode can now be stored in the registry
  (`lazy`, `grouped`, or `full`) instead of only being controlled by a process env
  var.
- **MCP request cancellation forwarding.** The gateway now proxies cancellation
  signals down to the active downstream request path, so canceled client work can
  stop instead of continuing pointlessly in the background.
- **HIL decision audit records.** Approval decisions now record the gate reason,
  decision kind, held duration, and a canonical `argsHash` without storing raw
  arguments.

### Changed

- **HIL approval failures are legible.** A dead or stale approval broker is reported
  as `unreachable`, distinct from a human timeout, and the gateway re-reads the broker
  descriptor once to self-heal the common app-restart/rebound-port race.
- **Lazy search recall improved.** Added dispute/chargeback and token/tokenize
  synonym coverage, improving the local recall fixture from 87% to 96% at 10.

### Fixed

- **Packaged Windows gateways escape MSIX filesystem virtualization.** The app and
  gateway now agree on the same real data directory, avoiding stale registry and
  approval files from Windows app-container redirection.
- **Several high-severity audit findings were closed.** The pass tightened external
  URL opening, catalog/import handling, and content-defense scanning, including a
  result-side evasion found during the app audit.
- **Release hygiene.** Version metadata now targets `1.5.1`, and local `.claude/`
  session artifacts are ignored so they cannot drift into release commits.

## [1.5.0] - 2026-07-05

A robustness release (the gateway, app, and Teams client now recover cleanly from
failure modes that used to fail silently), plus the Teams polish batch for the
Toolport for Teams launch.

### Added

- **Playground: cancel a stuck call.** A tool call that hangs now shows a live elapsed
  timer and a Cancel button, with a clear timeout message, instead of spinning on
  "Calling…" indefinitely.
- **Teams: automatic background sync.** A member's shared server set and security policy
  now stay current on their own (on launch and on a modest interval), so an admin's
  change reaches every member, not just those who click "Sync now".
- **Teams: synced servers grouped by state.** Servers your team shares now split into
  "Needs review" (on top, awaiting your enable) and "Active", so a fresh sync can't
  hide below the fold. (#190)

### Changed

- **Confirm before deleting saved credentials.** Clearing an OAuth token or removing an
  API key now asks first, matching every other destructive action in the app.
- **Catalog search failures are distinguishable from empty.** A registry or network
  error during a catalog search now shows an error with a retry, not a misleading
  "no results".
- **Search ranks the on-the-nose tool first.** In lazy discovery, a tool whose name
  matches your query exactly now wins near-ties instead of losing to a chattier
  description. (#189)
- **Teams and Playground polish.** The shared-server list is sorted, the Playground's
  invoke panel is hoisted above the fold, filters gained clear affordances, and the
  Playground shows proper empty/auth states. (#184, #196)
- **Catalog: removed the dead Railway entry** (its MCP endpoint 404s). (#195)

### Fixed

- **Crashed downstream servers recover automatically.** A stdio server that crashes or
  exits mid-session is now re-spawned on the circuit breaker's probe instead of staying
  dead until the client restarts (self-heal previously only fired when every server was
  down).
- **Teams: removed members are actually cut off.** A removed or demoted member's app now
  disconnects the team locally and refreshes their role on sync, instead of quietly
  keeping the team's servers and stale security policy.
- **Gateway tolerates an unsplit command.** A server config whose `command` is one
  string ("npx -y some-server" with no args array) now just works instead of failing
  with "cannot find the path specified (os error 3)". (#191)
- **Teams: restricted members get 304s again.** The app echoes the server's exact
  team-config ETag, restoring the not-modified fast path for members behind per-server
  access rules. (#192)
- **"Push my setup" no longer pushes the gateway itself** as if it were one of your
  team's servers. (#194)
- **The invite-code field no longer implies a `ci_` prefix** codes don't have. (#193)
- **Onboarding surfaces a failed health probe** on the final step instead of a
  green-looking finish over a server that never started. (#186)
- **Settings: security panels distinguish a failed poll from genuinely empty.** (#185)
- Under-the-hood hardening: the embeddings endpoint is time-boxed so a hung model falls
  back to lexical search, stdio reads are size-bounded, the tool cache is versioned so a
  stale cache from an older build is rebuilt, and shaped-result messaging no longer
  over-promises that a paged result is permanently retained.

## [1.4.0] - 2026-07-04

### Added

- **A full visual redesign.** Toolport moves to its brand palette, a deep navy
  ground with a single orange accent, applied consistently across every tab. Server
  health now reads as a colored word (not just an 8px dot), the Servers header is a
  scannable status bar, and the transport label is demoted to neutral so color means
  health, not metadata.
- **The connect flow shows the product.** Pointing a client that isn't connected yet
  now leads with a `client -> Toolport -> your servers` diagram and a clear call to
  action, instead of a wall of prose.
- **Tool identities are searchable and grouped by server.** Activity → Tool identities
  collapses hundreds of tools into per-server sections with a filter box: type a server
  name to see its whole block, or a tool name to jump to it.
- **A security posture summary in Settings.** The Security section opens with a
  one-line read of whether you're protected (guarded / partly / unprotected) and what's
  active, so you don't have to decode every toggle.
- **Pinned lazy-discovery tools now have a home.** When lazy discovery is on, Settings
  shows a Pinned prerequisites list of every tool you've pinned (with its server) and a
  one-click unpin, so the pin set is visible and manageable instead of being buried
  per-tool in Playground.
- **Tool-poison flags now show the matched text.** A flagged tool definition surfaces a
  short, de-obfuscated excerpt of exactly what tripped the scan, so the alert is
  verifiable instead of an opaque label.

### Changed

- **The Activity tab is calmer.** New/first-seen tools no longer flood the security
  lane; recurring notices collapse into a single counted row; the per-server stats table
  and discovery panel are collapsed by default; the recent-calls log shows all calls
  rather than defaulting to an errors-only view that read as "everything is failing."

### Fixed

- **First-seen destructive tools are no longer quarantined.** A destructive tool simply
  appearing for the first time is inventory, not a rug-pull, and no longer gets blocked
  behind a wall of re-approvals (the call is still gated by the block/confirm/approval
  policies). Legacy quarantine entries from the old behavior auto-clear.
- **No more spurious "integrity baseline lost" alarms** from an empty or mid-swap read
  of the shared pin file, while a genuinely truncated baseline is still treated as
  tampering (loud), not silently rebuilt.
- **A benign tool description no longer trips the poison scanner.** The stealth-directive
  check now requires a real concealment target, so a formatting note like "do not mention
  if a column is boolean" on a legitimate server is not flagged.
- **The connect view no longer describes buttons that aren't there,** and no longer lists
  Toolport's own gateway entry as one of the servers a client can reach (the managed count
  now matches the Servers list).
- **Corrected a Settings pointer** in the tool-identity history note that referenced an
  integrity-checking toggle which does not exist (integrity checking is always on).

## [1.3.0] - 2026-07-04

### Added

- **Discovery now shows why each tool ranked.** The lazy-discovery search trace
  (Activity → Discovery) records, per result, its rank, the query terms it matched
  (name vs description), whether it was a pinned prerequisite, and the ranker used
  (lexical vs semantic). You can now see not just which tools a search returned, but
  why, and what the model was handed.

### Changed

- **The gateway binary is now `toolport-gateway`** (was `conduit-gateway`; the macOS
  helper bundle is `ToolportGateway.app`). Existing client integrations keep working:
  detection and path resolution accept both names, macOS ships a compatibility symlink,
  and on launch Toolport re-points any client config still naming the old binary to the
  new one (each config is backed up first). Keychain and stored data are untouched: the
  keychain service, access group, master key, bundle id, and data directory are all
  unchanged, so no secrets or servers are lost across the update.

## [1.2.0] - 2026-07-03

### Security

- **Closed several bypasses in the stdio spawn guard** (the supply-chain check that
  refuses code-smuggling launch args on a spawned server). Two rounds of adversarial
  review found a booby-trapped (team- or registry-sourced) config could still reach code
  execution through: wrapper programs (`sudo`/`time`/`flock`/`busybox`/... run the real
  program from their args); Deno/Bun remote execution (`deno eval`, `deno run`/`serve`
  and `bun run` of an `http(s)://` / `npm:` / `jsr:` target); several unlisted
  interpreters (`osascript`, `elixir`, `lua`, `Rscript`, `julia`, `awk`, ...) and
  `cmd /c` on Windows; an attached `node -r./x` preload; and code-injecting env vars in
  the config (`LD_PRELOAD`, `DYLD_INSERT_LIBRARIES`, `BASH_ENV`, and preload/eval options
  inside `NODE_OPTIONS` / `RUBYOPT` / `JAVA_TOOL_OPTIONS`). The guard now catches all of
  these. `env VAR=val <cmd>` still works (the assignments are screened and the real
  command is checked), and normal launchers (npx/node/python/docker, benign env/tuning
  vars) are unaffected.
- **Agent-control enable/disable now respects the client's scope.** In HTTP mode a
  registered client could call `toolport_enable_server` / `toolport_disable_server` on
  a server outside its allowed set (toggling another tenant's server), and a "no server
  matches" error listed every server in the registry across tenants. Both the lookup and
  that "Known servers" list are now filtered to the client's scope, so an out-of-scope
  server is indistinguishable from a non-existent one. (Only reachable when the global
  "Allow agent control" opt-in is on.)
- **Agent-control toggles are audited with proof of the scope decision.** Each
  `enable_server` / `disable_server` attempt writes an `agent_control.server_toggle`
  audit record (client, profile, requested target, decision, and whether the lookup was
  scoped). A denied out-of-scope attempt records `resolvedServerId: null`, so the audit
  itself carries the guarantee that the denial never resolved or named an out-of-scope
  server.
- **`fetch_result` is now scoped to the client that produced the result.** In HTTP mode
  one gateway serves every registered client from a shared result cache with sequential
  `r{n}` cursors, so a scoped client could read another client's large-result body by
  guessing a cursor (`fetch_result` was the one data path that skipped the client scope
  check). It now only returns a result to the client that stashed it, with the same
  "unknown or expired" answer for anything else so cursors can't be probed.
- **A malformed `fetch_result` can no longer crash the gateway.** A pathological `len`
  overflowed the paging math into an invalid byte slice that panicked; on the stdio
  transport (no panic guard) that took down the whole gateway process. The offset math
  now saturates.
- **The stdio transport now catches handler panics like the HTTP one already did.** A
  panic while handling a request returns a JSON-RPC internal error for that request and
  keeps the gateway running, instead of unwinding out and dropping the whole MCP
  connection (defense-in-depth for the primary local transport).
- **HTTP clients are scoped on resources and prompts too.** A registered HTTP/OpenAPI
  client scoped to a subset of servers could still read _any_ connected server's
  resources and prompts (`resources/read` / `prompts/get` ignored the scope); they now
  enforce the same allowed-server set as tool calls.
- **Closed three tool-supply-chain detection gaps** from an internal audit: a tool's
  `outputSchema` is now poison-scanned (not just drift-hashed); injection in a result's
  `structuredContent` is flagged even when a text block already flagged; and a corrupt
  quarantine file no longer silently re-exposes quarantined tools (it's preserved and
  logged instead of failing open).
- **Gateway durability + auth hardening:** the registry is fsync'd before its atomic
  rename (no truncated file on a crash), an empty `Bearer` token is rejected instead of
  looked up, and the per-client token check is constant-time.
- **The approval / confirm gate now fails closed on an unresolved tool.** If the gateway
  can't tell from its cached catalog whether a tool is destructive, it re-checks the
  live tool list and, if the tool is still unknown, treats it as destructive (held for
  approval or confirmation) instead of letting it through unheld.
- **The injection scan is bounded.** Content-defense now caps the bytes it inspects per
  tool result (512 KB), so a hostile server returning a huge payload can't pin CPU.
  Realistic results are far under the cap, so detection is unaffected in practice.

### Added

- **Pi coding agent is now a supported client.** Toolport detects Pi, imports its
  configured MCP servers, and installs/removes the gateway entry in Pi's global config
  (`~/.pi/agent/mcp.json`), the same one-click flow as Cursor and the other clients.
  (Requested on the r/LocalLLaMA launch thread.)
- **Pin a tool as a lazy-discovery prerequisite.** Mark a load-bearing tool (auth,
  list-before-act, or one whose description doesn't match the model's keywords) with the
  pin toggle in the tool list, and lazy-discovery search will always surface it with its
  full schema, regardless of the query's match score, so it's never hidden behind
  discovery. Pinned tools stay scoped to the client and are capped so a large pin set
  can't itself bloat a result. (Requested on the r/LocalLLaMA launch thread.)
- **Tool identities (capability provenance).** A new Activity panel shows what each
  model-visible tool name actually maps to: its source server and the profiles that
  enable it, the pinned definition fingerprint drift detection checks against, and when
  the tool was first seen / last changed. Prefixing helps the model pick a tool; this
  lets a human verify what crossed the boundary. (The integrity baseline now tracks
  first-seen / last-changed per tool to power it.)
- **Teams can require human approval org-wide.** A team admin can turn on "Require
  human approval" in the Teams policy, and every member's gateway then holds gated
  tool calls for a person to approve. Like the other org policies, it's tighten-only:
  it can force approval on for the team but can never turn a member's own setting off.
- **Discovery panel: see what lazy discovery searched, and what it saved.** Activity now
  records each tool search the model ran: the query, which tools matched, and the
  tool-definition tokens the results cost that turn versus loading the whole catalog.
  Because Toolport is in the request path, those figures are measured, not estimated.
  Local and bounded, and it stores tool names only (never arguments or results).

### Changed

- **The HTTP gateway now handles requests concurrently.** Each request runs on its
  own worker, so a slow downstream server or a tool call held for human approval (up
  to two minutes) no longer blocks other requests, live setting toggles, or
  server-config reloads. The dispatcher already released its locks before the
  downstream call and the approval wait; the accept loop now hands each request to a
  worker instead of serving them one at a time.
- **Clearer client list and import view.** The client sidebar now surfaces connection
  state as the signal instead of the import backlog: connected clients read as a plain
  status (the count of importable servers moved to a small badge), and only
  not-connected / not-found / error clients carry a status word, so the one client that
  isn't wired up stands out instead of being buried under a wall of "connected". In a
  client's detail, "Move config in" is now the emphasized action (it's the real cutover
  that saves context), "Import" is clearly framed as a copy, and a note warns when
  importing into an already-connected client would load its tools twice. The profile
  scope dropdown was also widened so "All enabled servers" is no longer clipped.

### Fixed

- **macOS: monochrome menu-bar glyph, and no more Dock-and-menu-bar at once.** The
  tray now uses a template image (the Toolport porthole mark), so macOS tints it to
  match every other menu-bar item instead of showing the full-color app icon. And the
  Dock icon appears only while a window is open: closing to the tray (or auto-starting
  hidden at login) drops to the menu bar alone, and reopening restores the Dock icon.
- **Approval prompt is keyboard- and screen-reader-accessible.** When a held call
  appears, focus moves into the prompt, Escape denies the oldest pending call, and
  the count is announced. Also removed a brief flicker where a just-decided row
  could momentarily reappear.
- **The approval countdown is now exact.** It counts down to the broker's real
  fail-closed deadline instead of approximating from when the overlay first appeared,
  so the timer matches the moment the call actually auto-denies.
- Large tool results paged via `fetch_result` no longer re-scan the whole cached
  body on each page.
- A failed confirmation (e.g. removing a server) keeps its dialog open cleanly
  instead of surfacing an unhandled error.
- **Enable-all / Disable-all respects the current filter.** With a search filter active,
  the bulk toggle now acts only on the servers you can see instead of every server, and
  it's gated on its own busy state so it can't be double-fired. The profile delete dialog
  also names the profile it's about to remove.
- **Activity and the sidebar now report the same tokens-saved figure.** They each had a
  separate formatter that rounded differently, so the same number could read as, say,
  "1.2M" in one place and "1.23M" in another; both now use one shared formatter.
- **A failed health probe no longer shows a green "Refreshed".** If the manual refresh
  reloads your servers but the health check itself throws, it now says so instead of
  reporting success.

## [1.1.0] - 2026-07-02

### Added

- **Human-in-the-loop tool approval (opt-in).** With "Require human approval" on, Toolport
  holds any destructive or untrusted-server tool call and raises a desktop notification until
  you approve or deny it in the app. Fail-closed: if no decision is made in time, the call is
  denied. Off by default.
- **Runs in the tray / menu bar.** Closing the window now keeps Toolport running in the
  background (system tray on Windows, menu bar on macOS) so it can hold tool calls for approval
  while you work; the tray tooltip shows how many are waiting. Quit explicitly from the tray menu.
- **Launch at login (opt-in).** Start Toolport hidden in the tray when you sign in
  (Settings > General).

### Changed

- **Security notices are tiered by severity, so real threats aren't buried.** Risky
  tool-definition drift (a destructive tool changing, a tool dropping a readOnly/destructive
  safety annotation, or poisoned content) stays a loud, actionable notice; benign vendor
  revisions move to a quiet, collapsible "Recent tool changes" history. Dismissals now stick
  across restarts, and duplicate notices from multiple clients are collapsed.

### Fixed

- Cleaned up leftover "Conduit" references in a few spots (the Teams connect URL placeholder,
  the "download from releases" link, and the exported setup filename).

## [1.0.1] - 2026-07-02

### Fixed

- **Windows: upgraders now show "Toolport" in the Start menu.** After the rename, an
  in-place update from Conduit left the old "Conduit" shortcut and green icon behind
  (the bundle identifier is intentionally unchanged so your data and secrets carry
  over). The installer now removes that stale shortcut so the Start-menu entry and
  icon match the app.
- **Settings: clearer "Allow agent control" note.** It now states your destructive-tool
  block always stays yours, instead of referencing a toggle by position (which had
  since moved).

## [1.0.0] - 2026-07-02

- **Renamed Conduit to Toolport.** Visible names, the app title, and the meta-tools
  (`toolport_status`, `toolport_search_tools`, `toolport_call_tool`, ...) are now
  Toolport; the old `conduit_*` meta-tool names keep working as aliases. Internal
  identifiers (the `conduit-gateway` binary, the data directory, keychain entries, and
  `CONDUIT_*` environment variables) are unchanged, so existing installs upgrade with no
  loss of servers or saved secrets.
- **Security: confidence scoring + new injection categories.** The tool-poisoning /
  content-defense scanner now combines signals into a weighted confidence score
  (surfaced on security events) and adds three detection categories (role-jailbreak,
  system-prompt exfiltration, chat-template delimiter injection). Existing signatures
  and behavior are unchanged.

## [0.9.4] - 2026-07-01

### Added

- **Registry backup and recovery.** A `registry.json.bak` sibling keeps the
  last-known-good server list; Conduit recovers from it if `registry.json` is deleted
  or corrupted, so a bad write or an accidental wipe no longer loses your servers.

### Fixed

- **Per-server head-of-line blocking.** The gateway releases the per-server lock during
  a downstream backoff, so one server's 429 rate-limit no longer stalls other concurrent
  calls to that same server.
- **Retry-After clamp.** A downstream's `Retry-After` header is capped to the backoff
  cap (10s), so a misconfigured or hostile server can't park a call for minutes.

### Docs

- Codex setup walkthrough in the README.

## [0.9.3] - 2026-07-01

### Security

- **macOS: no more keychain prompts on update.** Secrets now live in the macOS
  data-protection keychain under a team-scoped shared access group, and the gateway
  ships as a nested notarized helper that shares that group. The gateway reads the
  secrets the app saved with no password prompt, even across app updates (the repeated
  "Conduit wants to use your confidential information" dialog is gone). Secrets still
  never touch disk.

### Added

- **Quarantine-on-drift.** High-risk tool-definition changes (a poisoned definition, or
  a destructive tool that changed or newly appeared) are blocked until you re-approve.
- **Headless encrypted-file secret backend** (`CONDUIT_SECRET_KEY`) for server/self-host
  use where no OS keychain is available.

### Changed

- Teams pricing is $12/seat (was $20). Smaller initial bundle via code-splitting.

## [0.9.2] - 2026-06-30

### Added

- **Catalog: configure-on-add** (enter keys while adding a server), **self-hosted
  servers** (n8n, Langfuse), and more entries (DataForSEO, Chrome DevTools, Railway,
  Twilio, Postiz).
- **Per-call confirmation for destructive tools.**
- **Paste a config snippet** to auto-fill the Add Server dialog.

### Fixed

- Remote servers refresh an expired OAuth token on a mid-session 401 and retry, no manual
  reconnect.
- Teams only soft-syncs servers the member opts into (no silent RCE from team config).

## [0.9.1] - 2026-06-29

### Added

- **New stack: Web scraping & automation.** An eighth role bundle (Firecrawl,
  Tavily, Playwright, Browserbase, Apify) for agents that search, scrape, and
  drive real browsers.
- **Share a stack as a link.** The Share dialog turns your selected servers into
  a `conduitmcp.app/s/...` link. The page unfolds the stack with a rich preview
  card, and its "Open in Conduit" button deep-links straight into the import
  review (with a copy-the-code fallback). Secrets are never included, and copy /
  save-to-file still work for offline sharing.

## [0.9.0] - 2026-06-29

### Added

- **Stacks: role-based server bundles.** Pick what you work on (full-stack web,
  backend & data, infra & DevOps, AI & ML, product & design, founder, research)
  and Conduit sets up a matching set of MCP servers in one click. Stacks appear at
  the top of the Catalog, and the first-run wizard now leads with a "What do you
  work on?" picker. Each server that needs a credential shows a direct "get key"
  link to the right token page.
- **Selective sharing.** Share a chosen subset of your servers as a stack instead
  of your whole setup (secrets still stripped; the recipient previews before
  importing).
- **Roo Code plugin-hosted server detection.** Conduit now surfaces Roo Code's
  plugin-provided MCP servers (read-only), matching the existing Cursor behavior.
  Thanks @leemeo3 (#50).
- **New catalog servers:** Linode (Akamai) cloud, and Qdrant (vector store for RAG).

### Fixed

- The scoped-client scope picker in Settings rendered an unthemed white dropdown
  in dark mode; it now uses the app's themed select.

### Internal

- Groundwork for concurrent tool routing (per-server interior mutability in the
  router; no behavior change yet), and a fix for an XDG env race that could flake
  a path test on CI.

## [0.8.0] - 2026-06-28

### Added

- **Multi-tenant HTTP bridge (per-client scoping).** Register HTTP clients in
  Settings → Integrations, each with its own bearer token and profile. One bridge
  process serves them all and resolves every request's token to its own set of
  servers, so (for example) two Open WebUI instances can see entirely different
  tools. The bridge connects the union of every registered client's profile, then
  filters each request (tools/list, search, call, status, and the OpenAPI spec)
  down to exactly what that token is allowed to see.
- **Resources & Prompts in the Playground.** New Tools / Resources / Prompts tabs:
  list a server's resources and read one, or fill a prompt's arguments and render
  it, exercising the full MCP surface Conduit proxies, not just tools.
- **Per-client scope, persisted and editable.** A connected client now shows its
  effective scope ("sees the 'Billing' profile, 3 servers"), and you can re-scope
  it in place without disconnecting.
- **Test connection in the add/edit server dialog.** Verify a server (and its
  secrets) actually connects before saving, alongside per-transport validation
  and a duplicate-name warning.
- **Activity error detail.** Failed tool calls now record and show the failure
  message and per-call latency; click a failed row to see why it failed.
- **Continue** client support (`~/.continue/config.yaml`). Thanks @BharadwajKanneveti (#49).
- The OpenAPI spec is now complete: a `servers` block, a `bearerAuth` security
  scheme, and real error responses, so OpenAPI clients can model auth and failures.

### Changed

- **HTTP bridge auth tightened.** Once any scoped client is registered, the bridge
  rejects unauthenticated requests even when no global token is set. CORS no longer
  reflects the caller's Origin or sends credentials, and cross-site browser
  requests are refused outright.
- **Downstream HTTP calls now retry** safely on a connection failure or a 429
  (honoring `Retry-After`) with capped backoff, never on a 5xx, since an MCP tool
  call may already have executed.

### Security

- Constant-time comparison for the bridge bearer token.
- The SSRF connect-guard now also blocks IPv6 link-local and cloud-metadata
  addresses (including the AWS IPv6 metadata address), not just IPv4 169.254.x.
- Client-config reads and backups reject non-regular files (devices, FIFOs) and
  cap size, so a crafted or symlinked config can't exhaust memory or disk.
- A scoped HTTP client's `conduit_status` no longer reveals other tenants'
  server names, commands, URLs, or tool counts.
- The placeholder-ID guard no longer blocks legitimate values like "todo" or
  "string" on content fields (only identifier-typed params).

### Removed

- **"Add to catalog" (promote-to-catalog).** It only pinned a server you already had into
  a local discovery view, with no sync or sharing, so it added clutter without real value.
  Browse Catalog still does what matters: discover and add new servers (curated set + live
  MCP-registry search).

## [0.7.0] - 2026-06-28

### Added

- **Native HTTP/OpenAPI transport.** Run the gateway with `conduit-gateway --http <port>`
  (or `CONDUIT_HTTP=<port>`) and it serves an OpenAPI spec plus a POST endpoint per tool,
  so Open WebUI and any OpenAPI tool client connect straight to Conduit with no mcpo,
  proxy, or Python bridge. It uses the same request path as stdio (one code path, two
  transports), binds both IPv4 and IPv6 loopback, and sends CORS headers so browser
  clients work. See [docs/openwebui.md](docs/openwebui.md).
- **One-click Open WebUI / HTTP endpoint toggle** in Settings -> Integrations. The app
  supervises the gateway, shows the URL to paste, verifies it actually started, and shuts
  it down when you quit.
- **Self-resolving multi-step tool calls.** When a model invents a placeholder identifier
  (e.g. `teamId: "your_team_id"`), the gateway refuses it before the downstream call and
  points the model at the right list/get tool on the same server to source the real value
  (resource-aware: a missing `teamId` suggests the team-listing tool first). The same
  recovery hint is appended whenever a call fails.

### Security

- **The HTTP endpoint now requires a bearer token.** The app auto-generates one, shows it
  in Settings -> Integrations, and you paste it into the client (Open WebUI: the tool
  server's API key / Bearer auth). This closes a credential-CSRF: the `localhost` bind does
  not stop a web page open in your browser from POSTing to the port and running your tools,
  but the token does. The gateway also refuses to bind a non-loopback host
  (`CONDUIT_HTTP_HOST=0.0.0.0`) without a token, caps request bodies, and sanitizes
  reflected headers so a crafted request can't inject or crash a listener.

### Changed

- **Windows installers are now code-signed** via Azure Trusted Signing (publisher name
  shows; SmartScreen reputation still builds with downloads).

## [0.6.0] - 2026-06-27

### Changed

- **The server list is a dense, scannable list now.** The bulky three-column cards are
  replaced by compact grouped rows: toggle, status, name, source, tool count, and
  transport on one line, with the command and per-server actions (secrets, duplicate,
  edit, remove) one click away in an expandable drawer. Needs-attention and disabled
  servers get their own collapsible groups (disabled starts collapsed). Roughly 2-3x
  denser at 20+ servers, and the row actions are real keyboard-reachable buttons now.
- **The catalog browse view is grouped by category.** The default view organizes the
  curated set into sections (Code & infrastructure, Databases, Search & knowledge, Web &
  automation, Apps & productivity, Local tools) instead of a flat grid; search stays flat.
- **Consistent accent colors.** Success, warning, info, and "yours" now come from four
  semantic tokens (one shade each) instead of emerald/amber/violet/sky drifting across
  300/400/500, so the same meaning renders identically in every view.
- **A calmer Activity page.** The tool-security panel is collapsible and each notice can
  be dismissed once reviewed; the raw call log is collapsed by default and filtered to
  errors first, so the per-server stats table stays the headline. The "has secrets" key
  icon on server rows is gone (it was a non-interactive indicator that looked clickable).
- **Global policy moved to a Settings view.** Lazy discovery, Block destructive tools,
  and Allow agent control now live in a dedicated Settings tab (grouped Discovery /
  Security) instead of being buried atop the Playground, which is now a clean
  tool-testing surface.

### Added

- **Three more catalog servers:** Perplexity, Kubernetes, and Todoist.
- **A confirmation step before destructive actions.** Removing a server, deleting a
  profile, disconnecting a client, or leaving a team now asks first and says what
  survives (your secrets stay in the keychain, your own servers are untouched).

### Fixed

- The manual Refresh always confirms now ("Refreshed"), even when a health probe is
  already running, so the click is never silent.
- Hardened the first-run wizard's resume-after-catalog flow against future regressions.
- A refresh failure no longer wipes a working server list; it keeps what's on screen and
  toasts instead. The full-screen error is reserved for the initial-load failure.
- The catalog browse view shows a loading skeleton and a retryable error state instead
  of silently collapsing to "Catalog unavailable."
- Dialogs cap their height and scroll, so a server with many env vars or secrets can't
  push the Save and Cancel buttons off-screen.
- Accessibility: screen readers now get the selected view (aria-current) and toggle
  state (aria-pressed), the active sidebar item reads clearly, and long names truncate
  instead of overflowing their rows.
- Consistent transport pills across every server list, plural-correct labels ("1 tool"),
  and several user-facing strings tidied up.
- The server row no longer nests its toggle and Authenticate buttons inside a clickable
  button. Mouse users still click anywhere on the row to expand; keyboard and screen
  reader users get a dedicated chevron button with proper `aria-expanded`.

## [0.5.2] - 2026-06-27

### Added

- **More one-click catalog servers.** Added MongoDB, Elasticsearch, Airtable, Exa,
  Tavily, Apify, Browserbase, and the Sequential Thinking, Memory, and Time reference
  servers, every package name verified.

### Changed

- **A calmer Servers header.** The duplicate Browse catalog button is gone (it's
  already in the sidebar), Search and Add server stay up front, and the occasional
  actions (Import, Enable/Disable all) move into a `...` overflow menu so the header no
  longer crowds on narrow windows. Thanks @BharadwajKanneveti.
- **One Refresh, not two.** The header's Refresh button now reloads servers, clients,
  and health in a single action and reports an "N of M servers healthy" summary, so the
  separate Check health action has been folded into it.

### Fixed

- **Onboarding no longer drops you mid-setup.** Browsing the catalog from the first-run
  wizard used to end onboarding before the Connect-a-client step; it now resumes there
  when you return, so new users don't silently skip connecting a client.
- **Onboarding tells the truth about broken servers.** The final step now probes the
  servers you just added and flags any that can't start (usually a missing runtime like
  Node or Python), instead of always declaring "you're set up."

## [0.5.1] - 2026-06-27

### Fixed

- **macOS: the keychain prompts are gone.** The `conduit-gateway` helper that your
  AI clients launch now reads your vaulted secrets (API keys, OAuth/bearer tokens)
  with no keychain password prompt. Newly saved secrets get this automatically;
  existing ones are upgraded on first launch. (Done with a trusted-application ACL
  granting both the app and the gateway access, since the modern entitlement
  approach can't work for a standalone helper binary.) Thanks @bradhallett for
  tracing the root cause.

## [0.5.0] - 2026-06-27

A security-hardening release. Conduit tightens the whole tool-trust boundary,
caps and filters what the gateway will fetch and sync, and adds accessibility and
UI polish.

### Fixed

- **The sidebar action bar stays put.** It's pinned to the bottom of the server
  list and always visible instead of appearing only when you scroll to the end,
  and undetected clients collapse under a disclosure so the list stays short.

### Security

- **Hardened the anti-agentjacking scan.** Tool results are normalized before
  scanning (lowercase, invisible/zero-width/bidi stripping, homoglyph and
  full-width folding) and base64-decoded payloads are scanned too, so injection
  text can't slip past with Unicode tricks or encoding. Nested `structuredContent`
  is scanned as well.
- **Rug-pull detection covers more of the tool definition.** Fingerprints now
  include `outputSchema` and `annotations` (version-tagged), so a server can't
  quietly change those behind an already-approved tool.
- **Integrity pins fail closed.** A corrupt or tampered pin baseline now raises a
  security event instead of silently resetting to trust-everything.
- **Blocked RCE/SSRF from synced team config.** Team sync drops stdio/command
  servers (remote code execution) and private-host URLs (SSRF); only public remote
  servers sync. The gateway also stops following HTTP redirects.
- **Capped downstream responses.** The gateway limits how much it reads from a
  downstream MCP server (16 MiB), so a hostile or runaway server can't exhaust
  memory.
- **Validated catalog install specs.** Registry-supplied package IDs with shell
  metacharacters or leading dashes are rejected, remote URLs must be http(s), and
  the registry fetch is size-capped.
- **Teams/OAuth hardening.** HTTP timeouts, a malformed-config guard, and token
  cleanup after a failed connect.

### Accessibility

- **Respects "reduce motion."** When the OS prefers reduced motion, Conduit zeroes
  out spinners, pulses, dialog and tooltip zooms, and transitions.

### Internal

- **CI on every PR**: frontend build, Rust library tests, and a gateway build
  check now run on pull requests across the project.
- **macOS:** newer secrets use the ACL-free SecItem keychain path, with a one-time
  migration of older entries (#26). The fuller DataProtection-keychain change is
  still in progress (it needs a code-signing approach that works for the gateway
  sidecar), so prompts behave as before for now.
- Removed leftover Vite/Tauri scaffold files and shipped a real favicon.

### Thanks

- @bradhallett (#26) for the macOS keychain migration work.

## [0.4.2] - 2026-06-26

### Added

- **Conduit Teams (beta), desktop side.** A new Teams tab connects your local Conduit
  to a self-hosted Conduit Teams server and syncs a shared MCP server set into your
  registry. Keys never leave your machine: only the server set syncs, and you
  authenticate each server locally. Inert until you connect to a team.
- **Composio** in the curated catalog (connect agents to 1,000+ apps via MCP). (#23)

### Fixed

- **Custom API keys now reach HTTP servers.** A remote/HTTP server that uses a manually
  vaulted secret (e.g. a `BEARER` key) gets it injected as the bearer token, not just
  OAuth tokens, so "Manage secrets" works for HTTP servers. (#22)
- **Cleaner multi-account duplicates.** Duplicating a server produces collision-free
  names (`Server (2)`, `(3)`) instead of `Server 2`, with an "add another account"
  hint. (#24)
- **Hermes config keys.** Hermes `mcp_servers` entries are keyed by server name, so the
  config round-trips correctly. (#25)

### Internal

- **macOS secret storage moved to the SecItem keychain API** for new entries, which
  avoids the per-application ACLs behind repeated keychain prompts (#21). If you're on
  macOS and still see prompts, they're from entries created by older versions: clear
  Conduit's old entries in Keychain Access and re-authenticate to use the new path. A
  confirmed prompt-elimination claim is pending validation on signed release builds.

### Thanks

- @bradhallett (#21, #22, #23, #25) and @BharadwajKanneveti (#24).

## [0.4.1] - 2026-06-26

### Changed

- **Windows installers are now code-signed** via Azure Trusted Signing, so the
  SmartScreen "unknown publisher" warning is gone (reputation still accrues with
  downloads). macOS was already signed and notarized; Linux remains unsigned as
  usual. No functional changes from 0.4.0.

## [0.4.0] - 2026-06-26

A security + intent-search release: Conduit now covers the whole tool-trust
boundary (both tool definitions and tool results), searches by meaning, can be
driven by the agent on your terms, and supports two more clients.

### Added

- **Tool-definition integrity (rug-pull + poisoning detection).** The gateway
  fingerprints every tool when a server is first connected and diffs it on each
  refresh. If a previously-approved tool's definition changes, or a known server adds
  a tool (the signature of a "rug pull"), it records a security event. It also scans
  each tool's description/schema for injection-like content (tool poisoning / line
  jumping) when first seen or when it changes. Both surface as notices in the Activity
  view. Detection only, never blocks; on by default (`integrityCheck`), fully local.
  New `get_security_events` command + `security.jsonl`.
- **Content defense (anti-agentjacking).** The gateway scans untrusted tool _results_
  for injection-like content and, on a hit, wraps the offending text with a provenance
  marker ("external data, not instructions") before the agent sees it, plus records a
  security notice. Information-preserving (the original text stays inside the marker),
  only flagged results are touched, never blocks. On by default (`contentDefense`). The
  result-side companion to the definition-side integrity checks.
- **Semantic tool search (optional).** `conduit_search_tools` can blend embedding
  similarity into its lexical ranking so paraphrased needs surface the right tool, not
  just keyword matches. Off by default (`semanticSearch`); point it at any
  OpenAI-compatible `/v1/embeddings` endpoint. Tool embeddings are cached on disk; on
  any failure it falls back to pure lexical, so it can only add signal, never degrade.
  New `benchmark/retrieval.mjs` measures retrieval recall (lexical vs semantic).
- **Controllable MCP (opt-in agent control).** A new _Allow agent control_ switch
  (off by default) lets an agent enable or disable servers through the gateway
  (`conduit_enable_server` / `conduit_disable_server`). The destructive-tool block
  stays user-only, so granting it can't let an agent escalate past your governance;
  the app watches the registry and reflects an agent's change live.
- **Two more clients / catalog entries.** **Hermes** (NousResearch Hermes Agent, YAML
  `mcp_servers` in `~/.hermes/config.yaml`) is now supported, bringing the total to
  **20 clients** (#20). **Firecrawl** (#19) and **OpenRouter** (live model
  intelligence) were added to the curated catalog.

### Changed

- Benchmark suite: added a graded server-sweep harness (`bench-sweep.mjs`) that grades
  answers for correctness, not just completion, and expanded `token-cost.mjs`
  (context-window share, scaling curve, per-tool distribution, multi-volume dollar
  tables). Headline numbers re-measured on a frontier model: up to ~91% fewer total
  tokens at the same graded task success.

### Fixed

- The Playground policy toggles lay out as an even responsive grid instead of
  orphaning the third switch onto its own row.

### Internal

- Release pipeline wired for Windows Authenticode signing via Azure Trusted Signing,
  gated and inert until the signing secrets are configured (changes nothing until the
  certificate is ready).

## [0.3.18] - 2026-06-25

### Added

- **Ask your agent what Conduit is saving you.** `conduit_status` now reports the
  tokens lazy discovery has kept out of context, a dollar estimate at Claude Sonnet
  input rates, the number of tool-list loads, and your biggest catalog collapse.

### Changed

- The in-app savings model picker and the public calculator group models by provider
  (Anthropic, OpenAI, Google), with a custom-price option on the calculator.

### Fixed

- Native select dropdowns render readable in the dark theme (no more light text on a
  light popup).

## [0.3.17] - 2026-06-25

### Added

- **Token economics card.** The Activity tab shows the dollar value of what lazy
  discovery has saved you, with a model-price selector and a one-click Share that
  copies a "Conduit saved me ~~X tokens (~~$Y)" snippet.

### Security

- Hardened three findings from an internal audit: OAuth PKCE/state generation now
  fails loudly instead of silently returning zeros if the OS RNG is unavailable;
  file writes use a unique atomic-write temp name (no torn writes under concurrent
  writers); and a saved bearer token is refused over non-HTTPS to a public host.

## [0.3.16] - 2026-06-25

### Added

- **Live tool refresh.** When a connected server changes its own tool set
  mid-session (via `tools/list_changed`), Conduit re-queries it in place, so new
  or removed tools reach your agent without a restart.
- **Always-on diagnostics.** A size-capped gateway log of connection events, plus
  a one-click **Copy diagnostics** button that bundles your version, OS, a
  secrets-stripped server summary, and the recent log, ready to paste into a bug
  report.
- **BoltAI** is now a supported client (18 total), thanks to a first-time
  contributor (#18).

## [0.3.15] - 2026-06-23

### Fixed

- Clean, all-platforms build of the tokens-saved counter. v0.3.14's Linux job was
  OOM-killed mid-compile, leaving no Linux build or updater manifest; the pipeline
  now gives the Linux runner enough disk and swap, so auto-update works on all four
  platforms again.

## [0.3.14] - 2026-06-23

### Fixed

- The v0.3.12 "tokens saved" counter was missing from the release binaries (a CI
  build cache compiled a stale library from before the command existed). The
  pipeline now builds the workspace from scratch, so the counter ships.

## [0.3.12] - 2026-06-23

### Added

- **"Tokens saved" counter in Activity.** A running estimate of the
  tool-definition tokens lazy discovery has kept out of your agent's context, with
  tool-list loads, your biggest catalog collapse, and since-when. No setup.

## [0.3.11] - 2026-06-22

### Improved

- **Cleaner search index.** The gateway strips boilerplate and stopwords from tool
  descriptions and queries before indexing, so `conduit_search_tools` ranks on the
  words that actually distinguish one tool from another.

### Added

- **BENCHMARK.md** with a reproducible harness: ~97% less tool-definition overhead
  per request and ~90% fewer total tokens at the same task success rate (3 servers,
  62 tools, local model, repeated runs).

## [0.3.10] - 2026-06-22

### Improved

- **Tool search ranks the right tool more often.** When a query mixed a common word
  with a specific one (e.g. "list products"), keyword matching could surface a generic
  "list" tool instead of the products one. Search now tokenizes queries and tools
  (splitting camelCase, light stemming), weights matches by how rare the token is so a
  specific word like "products" outweighs a common one like "list", and bridges a small
  synonym map (mail/email, get/list, team/org). The agent finds the intended tool with
  fewer searches.

## [0.3.9] - 2026-06-22

### Added

- **Two more clients: Jan and Goose** (17 supported in total). Jan uses the standard
  `mcpServers` JSON; Goose is the first YAML client, its MCP servers live under a
  top-level `extensions:` map in `config.yaml`. Both detect, connect with one click,
  and import existing servers, with the same no-wipe safeguard as Zed (config.yaml
  also holds Goose's model settings and built-in extensions).

### Fixed

- **Required tool parameters now work from grammar-constrained local clients.** Some
  local runtimes (e.g. Jan) force the model's output to match the tool schema, and
  `conduit_call_tool`'s `arguments` declared no properties, so the model could only
  ever emit an empty `{}`, making a required param (e.g. Vercel's `teamId`) impossible
  to pass. `arguments` now accepts arbitrary properties, and the gateway also tolerates
  models that put params at the top level instead of nesting them under `arguments`.
- A stdio server entry now always writes `args` (even empty); some clients reject an
  entry whose `args` key is missing. An empty `command` string is treated as no command,
  so a remote/url server shipped with `"command": ""` isn't mis-read as stdio.
- The sidebar now fills the full window height instead of stopping at its content.
- Clearer messages when the onboarding starter list can't load (offline) and when a
  Linux box has no system keyring.

## [0.3.8] - 2026-06-21

### Improved

- **Faster, more decisive tool search, especially with local models.** Search now
  leads with the single best match and tells the model to call it; the remaining
  results come back as a compact menu (name + a one-line description, no schema)
  instead of every tool's full schema. A large result set drops from tens of KB to a
  few KB, so a model that re-reads its context each turn (local models especially)
  runs noticeably faster. Full schema for any other tool still comes from a scoped or
  exact-name search.
- **A loop-breaker for weaker models.** When a model re-searches and keeps landing on
  the same top tool, the gateway returns just that tool and tells it to call it,
  rather than letting the model spin on repeated searches. It only triggers on a
  repeated top result, so a capable model, or one legitimately exploring different
  tools, is never affected.

## [0.3.7] - 2026-06-21

### Added

- **Five more clients: Zed, LM Studio, Warp, Amazon Q, and Kiro.** Conduit detects
  each, installs the gateway with one click, and imports its existing servers.
  - Zed keeps MCP servers under `context_servers` in its `settings.json`, which is
    JSONC (comments and trailing commas) holding the user's whole editor config. That
    file is now read leniently so a commented config isn't mistaken for corrupt, and
    is **never replaced with an empty document on a parse failure**, so Conduit cannot
    wipe your settings.
  - LM Studio, Warp, Amazon Q, and Kiro use the standard `mcpServers` JSON shape at
    their respective config paths (`~/.lmstudio/mcp.json`, `~/.warp/.mcp.json`,
    `~/.aws/amazonq/mcp.json`, `~/.kiro/settings/mcp.json`).

### Fixed

- Client detection now reflects whether an app is actually installed, not merely
  whether an MCP config file happens to exist. The old "config file's parent dir"
  heuristic was wrong for some clients: Claude Code's config lives at `~/.claude.json`
  (parent is the home dir, which always exists, so it falsely showed as installed
  everywhere), and Warp's `~/.warp` only appears after its first file-based MCP use.
  Those clients now check an explicit install/data directory.

## [0.3.6] - 2026-06-21

### Fixed

- Lazy-discovery tool search is far more reliable on multi-server setups. A tool
  that exists could read as missing (so an agent would wrongly conclude a server
  was "read only"): the default result limit was too low with no signal that
  results were truncated, and one server with many matching tools could crowd out
  the rest. Search now returns more results, reports when it truncated and how to
  narrow, diversifies across servers, and accepts a `server` filter to scope or
  fully enumerate one server's tools. `conduit_status` now lists each server and
  its tool count.
- Tool search no longer blows up the agent's context: a few servers ship enormous
  input schemas (tens of KB each), and search returned the full schema for every
  result. It now bounds the total schema size (keeping the top result's full schema
  and returning the rest compact) and truncates long descriptions. Full schema/text
  for a specific tool is available by searching its exact name.

## [0.3.5] - 2026-06-21

### Security

- Importing a shared setup now previews exactly what it will run (each server's
  command, args, and url) and imports only on confirmation, and flags entries that
  spawn a shell. A shared config can no longer slip an unseen command past you.
- OAuth endpoints discovered from a server's metadata are rejected if they point at
  a private or loopback address while the server itself is public (SSRF guard);
  legitimate local servers are unaffected.
- Set an explicit Content-Security-Policy for the app window.

### Fixed

- Registry writes are atomic, so a crash mid-write can't corrupt your server set.
- A corrupt registry no longer silently makes every tool vanish: the gateway keeps
  serving the last good tool list and logs the problem.
- The user catalog and config backups are stored in one consistent location across
  packaged and unpackaged installs.

### Changed

- Onboarding's final step reflects what you actually set up and explains lazy
  discovery; the empty state offers a "Browse catalog" action; the New Profile
  dialog explains that profiles scope servers, not credentials.
- Clearer macOS OAuth guidance (shown before sign-in, not only after a failure).

## [0.3.4] - 2026-06-21

### Fixed

- Client config writes are now atomic (temp file + rename), so a crash or full
  disk mid-write can't truncate a client's MCP config.
- One unresponsive stdio server no longer stalls the whole gateway: the connect
  handshake fails fast (10s) instead of waiting the full 30s read timeout.
- Playground policy toggles report failures instead of silently reverting.
- The share-import file size is capped before reading.
- Updater "Check for updates" tells "up to date" apart from "couldn't check".
- macOS builds now publish auto-update artifacts; macOS auto-update was inert in
  v0.3.3 (the update manifest had empty macOS entries).

## [0.3.3] - 2026-06-21

### Added

- First-run onboarding wizard: detect clients, add your first servers (import,
  one-click popular starters, or the catalog), and connect a client. Re-run it
  anytime from the sidebar footer.
- In-app auto-updater: Conduit checks for new releases and can download, install,
  and relaunch itself, with release notes shown before installing.
- Share a setup as a `.json` file (in addition to the clipboard), with an optional
  name and description. Secrets are never included.
- Per-tool breakdown in the Activity dashboard, plus server and errors-only filters.

### Changed

- Reliability: gateway recovers from a poisoned lock, the audit log rotates so it
  can't grow unbounded, more tolerant SSE id matching, and a guard against
  overlapping health probes (which curbed macOS keychain prompt storms).

## [0.3.2] - 2026-06-20

### Added

- Signed and notarized macOS builds (Apple Silicon + Intel), alongside Windows
  and Linux, via a tag-triggered release pipeline.

## [0.3.0] - 2026-06-20

- First public release: local MCP gateway and manager with lazy discovery,
  per-agent profiles, the catalog, the tool playground, and the activity log.

[Unreleased]: https://github.com/tsouth89/toolport/compare/v1.6.2...HEAD
[1.6.2]: https://github.com/tsouth89/toolport/compare/v1.6.1...v1.6.2
[1.6.1]: https://github.com/tsouth89/toolport/compare/v1.6.0...v1.6.1
[1.6.0]: https://github.com/tsouth89/toolport/releases/tag/v1.6.0
[1.5.3]: https://github.com/tsouth89/toolport/releases/tag/v1.5.3
[0.3.16]: https://github.com/tsouth89/conduit/releases/tag/v0.3.16
[0.3.15]: https://github.com/tsouth89/conduit/releases/tag/v0.3.15
[0.3.14]: https://github.com/tsouth89/conduit/releases/tag/v0.3.14
[0.3.12]: https://github.com/tsouth89/conduit/releases/tag/v0.3.12
[0.3.11]: https://github.com/tsouth89/conduit/releases/tag/v0.3.11
[0.3.10]: https://github.com/tsouth89/conduit/releases/tag/v0.3.10
[0.3.9]: https://github.com/tsouth89/conduit/releases/tag/v0.3.9
[0.3.8]: https://github.com/tsouth89/conduit/releases/tag/v0.3.8
[0.3.7]: https://github.com/tsouth89/conduit/releases/tag/v0.3.7
[0.3.6]: https://github.com/tsouth89/conduit/releases/tag/v0.3.6
[0.3.5]: https://github.com/tsouth89/conduit/releases/tag/v0.3.5
[0.3.4]: https://github.com/tsouth89/conduit/releases/tag/v0.3.4
[0.3.3]: https://github.com/tsouth89/conduit/releases/tag/v0.3.3
[0.3.2]: https://github.com/tsouth89/conduit/releases/tag/v0.3.2
[0.3.0]: https://github.com/tsouth89/conduit/releases/tag/v0.3.0
