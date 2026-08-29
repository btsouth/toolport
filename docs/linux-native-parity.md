# Linux-native replacement audit

This inventory maps the replacement plan to current implementation and proof.
It is intentionally stricter than a feature list. A row is complete only when
the native shell has the behavior, reuses shared Rust policy where appropriate,
and has direct test or runtime evidence.

> **2026-08-29 deep audit:** a UI-affordance and runtime-behavior sweep (as
> opposed to the command-surface sweep the table rows were graded against)
> found 26 gaps; all were closed the same day. The "Remaining gaps" section
> below records what was built and the two deliberate leftovers.

| Requirement                                                                             | Native implementation                                                                                                                                                                    | Evidence                                                                                                | Status               |
| --------------------------------------------------------------------------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------- | -------------------- |
| Additive GTK shell without changing Windows or macOS packaging                          | `toolport-gtk` is behind the `gtk-desktop` feature; the Tauri entry point remains the default                                                                                            | Both Cargo feature sets compile; existing frontend build and tests pass                                 | Complete             |
| Omarchy theme and translucent surfaces                                                  | GTK CSS is generated from the active Omarchy palette and monitored for changes                                                                                                           | Theme parser and generated CSS tests; live Hyprland screenshots                                         | Complete             |
| Compositor-owned tiling and geometry                                                    | The window uses default size hints only and no center, maximize, or movement workaround                                                                                                  | Live Hyprland client state reports tiled, nonfloating windows at multiple sizes                         | Complete             |
| Safe registry startup and external refresh                                              | Startup runs the operational recovery path once; later views use read-only refresh and a file monitor                                                                                    | Recovery and read-only state tests; isolated corrupt-primary recovery test                              | Complete             |
| Server list, profiles, search, CRUD, enablement, and secrets                            | GTK calls `registry_controller` and shared keychain transactions; Add server fills from a pasted JSON, TOML, YAML, or CLI snippet and vaults pasted env values on save                   | Controller rollback and invariant tests; snippet feedback tests; native shell compile and runtime smoke | Complete             |
| Server health and authentication                                                        | Shared runtime covers bounded probes, bearer auth, OAuth, client credentials, and secret status; the authentication editor probes and names the scheme the server actually wants         | Runtime, OAuth, remote, vendor, and controller tests; probe summary test                                | Complete             |
| Client detection, import, connect, reset, disconnect, profile scope, and discovery mode | GTK uses the existing client readers and shared mutation controller                                                                                                                      | Client-file rollback, managed ownership, scope, and Shared HTTP tests                                   | Complete             |
| Pending approvals and desktop notifications                                             | Native broker host renders approvals and sends one secret-free `Gio.Notification` per pending call                                                                                       | Approval broker tests and native notification reconciliation                                            | Complete             |
| Tray and hidden lifecycle                                                               | StatusNotifierItem exposes Open, Pending approvals, and Quit; close and autostart keep the broker alive                                                                                  | Live D-Bus tray registration and hidden-launch smoke                                                    | Complete             |
| Activity, statistics, security events, traces, live inspection, clearing, and export    | Native Activity uses the shell-neutral observability controller and audit exporters, plus per-server and per-tool breakdowns, the savings counter, and the tool-identity provenance list | Activity view privacy tests, shared audit tests, stat line, grouping, and fingerprint tests             | Complete             |
| Rules and project rules                                                                 | Native view supports sets, imports, exact previews, opt-in, apply, and project mappings; drift is resolvable with re-apply, per-client overwrite, and pull into set                      | Existing rules engine tests (including the drift overwrite test) plus native build                      | Complete             |
| Agent permissions, guard, and activity hooks                                            | Dedicated native pages call the shared preview and apply paths                                                                                                                           | Agent permissions, guard, and hooks tests                                                               | Complete             |
| Safety settings, quarantine, approvals, routines, and folder routing                    | Native Settings uses effective team policy and shared controllers; quarantine offers per-tool release and one-pass re-approval across every blocked profile scope                        | Controller, integrity (`release_all`), approval, routine, and routing tests; bulk feedback tests        | Complete             |
| Shared HTTP endpoint and scoped clients                                                 | Native Settings supervises the bridge, copies explicit credentials, mints hash-only scoped clients, revokes them, and restores the endpoint after stale cleanup                          | Shared HTTP controller tests and package-upgrade reaper tests                                           | Complete             |
| Catalog and starter stacks                                                              | Native Catalog uses the bundled catalog, official registry search, and shared add controllers                                                                                            | Catalog and stack tests                                                                                 | Complete             |
| Playground tools, prompts, resources, overrides, pins, and calls                        | Native Playground calls the shell-neutral runtime controller                                                                                                                             | Playground controller and router tests                                                                  | Complete             |
| Teams join, approval polling, sync, review, leave, and push                             | Native Teams uses existing team transactions and reviewed push fingerprints, and shows the member-facing Team Instructions content, version, and per-client on-disk state                | Team policy, merge, consent, instruction, and push tests                                                | Complete             |
| First-run setup and replay                                                              | Native assistant covers MCP, rules, teams, starter stacks, reviewed import, direct client connection, and shared health checks                                                           | Onboarding marker test, 17 native tests, live rendered assistant smoke                                  | Complete             |
| Import, export, sharing, deep links, and native file dialogs                            | GTK handles setup files and both URL schemes using shared preview and import policy                                                                                                      | Isolated invalid-link launch test and shared controller tests                                           | Complete             |
| Diagnostics and data-folder integration                                                 | Native Settings uses the shared redacted diagnostics controller                                                                                                                          | Diagnostics tests and explicit clipboard or file-manager actions                                        | Complete             |
| Package-managed updates                                                                 | Native Settings directs users to Omarchy or pacman and contains no self-updater                                                                                                          | Source inspection and native feature build                                                              | Complete             |
| Arch payload and metadata                                                               | Staging and `PKGBUILD` include both binaries, desktop entry, URL handlers, AppStream, icons, license, and Agent Plugins archive                                                          | Desktop and AppStream validation plus isolated pacman lifecycle test                                    | Complete for preview |
| Data-preserving install, upgrade, rollback, and uninstall                               | Isolated fakeroot pacman transactions hash registry and client fixtures after every transaction                                                                                          | `scripts/test-linux-native-package-lifecycle.sh`                                                        | Complete             |

## Remaining gaps to full 1:1 (2026-08-29 audit)

All 26 gaps from the 2026-08-29 deep audit were closed the same day. What was
built, by audit item: (1) the approval card's browser button now goes through
`oauth::validate_web_url`/`open_web_url`, the same scheme + metadata-host guard
the shipping IPC boundary uses, and an invalid URL renders as a visible refusal;
(2) `registry_controller::migrate_client` is shared one-shot migration (guard,
import, config rewrite with backup, scope + managed record, rollback), with a
per-client "Move in N" action and moved/imported/backup feedback; (3) the
Servers list probes every enabled server in the background with per-row
Ready/Needs sign-in/Error status, an Authenticate CTA, copy-probe-error, a
posture line, and attention-first grouping; (4) the startup reaper runs at
launch plus a delayed pass, restores the bridge, and announces restart advice
by feedback and notification, with a durable per-app/pid list in Settings;
(5) quarantine has a 15-second watcher, sidebar count badge (with an honest
"?" unknown state), and OS notifications via shared
`integrity::quarantine_notification`; (6) the tray shows the live pending
count in menu and tooltip, approval notifications use the browser-action
wording and urgent priority; (7) the one-time tray hint fires on first close,
sharing the shipping marker; (8) Settings refreshes quietly every 15 s while
mapped; (9) team removal and blocked-as-unsafe counts surface as notices and
a notification; (10) window size/maximized persist via
`gtk-window-state.json` (position and min-size stay compositor-owned by the
tiling contract); (11) Activity ticks every 3 s while visible with
change-detection, server + errors-only filters with counts, and stale-vs-
failed feedback over retained rows; (12) security notices use shared
loud/quiet lane logic (`integrity::dedupe_security`,
`collapse_security_by_identity`, `security_key`) with per-notice and
dismiss-all durable dismissals and evidence excerpts; (13) discovery traces
expand into ranking/matched/token math and calls carry PII badges; (14) tool
identities are searchable with force-expanded matches, and the savings banner
has the dollar estimate, model picker, detail line, and share-to-clipboard;
(15) Playground gained cancel-with-elapsed-counter, reset-override,
copy-result, a tool filter, and a schema-driven typed argument form with an
edit-as-JSON escape hatch; (16) share/export take name, description, and a
server subset, with copy-JSON, save-to-file, paste-text import, per-item
setup-review selection (`sharing_controller::import_json_selected`), and
per-row shell-command/private-address warnings; (17) clients show restart-to-
take-effect notices, a collapsed Not-installed section, per-row import
counts, and discovery-mode explainers with an advisory recommendation;
(18) catalog entries show provenance tier, publisher, and a validated docs
link, and stacks disclose setup steps with credential links; (19) servers
have Duplicate with next-free-name, copy-probe-error, and a duplicate-name
soft warning; (20) rules name unsupported clients, re-preview any client,
call out refused writes with the reason, and render drift as a line diff;
(21) Settings has the token reveal toggle, the posture summary, the durable
restart panel, and the keychain-backed secret-stored/couldn't-check indicator
with retry; (22) Teams has the create/how-it-works/pricing/self-host links
and cancel-pending-join; (23) the one-off star prompt exists; (24) hidden
launch now requires a live StatusNotifierWatcher, not the blind-assumed tray;
(25) the native preview autostarts under its own `ToolportNativePreview`
entry so it cannot repoint the shipping shell's login launch (identities
merge at cutover); (26) an unpackaged build registers the `toolport://` and
`conduit://` handlers at runtime, like the shipping shell does.

Still open, deliberately: the full app-ID unification (item 25's cutover
half) happens at the replacement release; the approval card for routine
writes still shows pretty-printed JSON rather than the structured
name/risk/calls/dependencies breakdown (item 22, second half); and the
per-client "servers it can reach" chips and gateway-flow diagram (item 17,
second half) are not yet drawn - the same facts are visible on the card's
scope line and in the tooltips.

## Intentional Linux differences

- Appearance follows the active Omarchy palette instead of preserving the
  WebKit light, dark, and system selector. This is the native theme contract.
- Updates are owned by pacman or the Omarchy update flow. The Tauri downloader
  remains available only in the shipping cross-platform shell.
- The GTK binary and package retain preview identity while both Linux shells are
  installable side by side. Production identity changes only at the replacement
  release so desktop IDs and package files cannot collide early.
- Updater-only process recovery commands are not exposed as native UI. The
  package lifecycle and stale-gateway action replace that workflow.
- Clearing retained activity is one explicit action covering the call audit,
  discovery traces, inspector captures, and savings tally, instead of the
  shipping app's separate inspector and trace clears. The inspector buffer is
  additionally cleared on startup whenever live inspection is off.
- Migrating a client is two reviewed steps (import its servers, then connect
  it) rather than the shipping one-click migrate; both end in the same
  gateway-only client configuration with the same backups.

## Release gates not proven locally

- Build the selected tagged source archive in an Arch clean chroot.
- Replace `SKIP` with the signed release archive checksum.
- Choose the production package name, desktop application ID, binary alias, and
  conflict or replacement metadata for the Linux cutover.
- Run the stable and edge Omarchy beta, GNOME Wayland, KDE Wayland, and X11
  matrix for tray behavior, keyring prompts, fractional scaling, accessibility,
  and long-running resource use.
- Keep the Tauri Linux artifact available for rollback until the beta exit gate
  is satisfied.
