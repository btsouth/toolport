# Linux-native Toolport plan

## Goal

Ship a first-class Linux Toolport desktop shell that:

- looks intentional beside Omarchy applications and follows the active Omarchy palette;
- behaves like an ordinary Hyprland window, including tiling, moving, stacking,
  fullscreen, floating, workspace changes, and compositor-owned placement;
- remains usable as its tile becomes narrow or short;
- integrates with the launcher, tray, notifications, portals, scaling, and
  keyboard navigation;
- preserves the existing registry, secrets, gateway, client configuration, and
  approval behavior; and
- does not change the current Windows, macOS, or Tauri Linux builds until the
  native shell passes the replacement gates below.

GTK4 and libadwaita are implementation choices. The product requirement is the
experience above, not resemblance to a stock GNOME application.

## Non-goals

- Rewriting the gateway or registry format.
- Replacing the Windows or macOS desktop shell.
- Adding a persistent gateway service. Local AI clients continue to spawn the
  existing stdio gateway.
- Installing Omarchy hooks or modifying user-owned Omarchy configuration.
- Shipping both Linux shells as concurrently running applications. They share
  background state and must not compete for the approval broker endpoint.

## Safety model

Development is additive:

1. The existing `desktop` Cargo feature remains the default.
2. The GTK shell is behind a separate opt-in `gtk-desktop` feature and binary.
3. Shared logic moves out of the Tauri module only when the new shell needs it.
4. Each extraction lands with behavioral tests before either adapter changes.
5. Existing frontend, Rust, headless gateway, installer, and cross-platform
   tests remain required throughout the project.
6. Linux packages keep shipping the Tauri application until the native shell
   passes the beta and replacement gates.

The GTK and Tauri shells use the same registry and keychain identifiers. Any
new shell preference uses a small file in Toolport's existing data directory;
it must not change the registry schema for presentation-only state.

## Architecture

Keep the current Rust crate. It already builds as a library without Tauri and
contains the reusable registry, gateway, clients, security, rules, teams, and
audit modules.

Add:

- `toolport-gtk`, a Linux-only binary behind `gtk-desktop`;
- a Linux-native shell module containing application lifecycle, views, and
  Omarchy integration;
- a small shell-neutral event sink for approval, registry, team, and routine
  events when those features are ported; and
- controller functions only where `desktop.rs` currently combines business
  behavior with Tauri state.

Do not create a separate core crate in advance. Reconsider a workspace split
only after both shells reveal a stable boundary.

## Omarchy integration contract

The active foundational palette is read from:

`$XDG_STATE_HOME/omarchy/current/theme/colors.toml`

with `~/.local/state` as the XDG fallback. Toolport reads this file and monitors
it for changes. It does not edit the file or install a theme hook. The initial
roles are:

| Toolport role            | Omarchy color        |
| ------------------------ | -------------------- |
| Window background        | `background`         |
| Raised surface           | `lighter_background` |
| Recessed surface         | `dark_background`    |
| Primary text             | `foreground`         |
| Secondary text           | `muted`              |
| Focus and primary action | `accent`             |
| Selection                | `selection`          |
| Error and destructive    | `red`                |
| Success                  | `green`              |

Every value is validated as a six-digit hex color before it enters GTK CSS.
Missing, incomplete, or malformed themes fall back to a built-in accessible
palette. `mode = "light"` or `mode = "dark"` sets libadwaita's color scheme.

Omarchy-specific styling stays in one adapter so generic Linux support can use
libadwaita defaults when the runtime palette is absent.

## Responsive window contract

- Do not center, maximize, unmaximize, move, or restore position from the app.
- Set a reasonable default size as a hint only.
- Do not set the current Tauri minimum dimensions on the GTK window.
- Use adaptive navigation and breakpoints so the sidebar collapses and forms
  reflow in narrow tiles.
- Keep primary actions reachable at 480 by 360 logical pixels.
- Let Hyprland own placement and all tile geometry.
- Treat floating, fullscreen, stacking, and workspace moves as compositor
  operations with no application-specific branches.

## Delivery phases

### Phase 1: isolated foundation and platform proof

Deliverables:

- opt-in `toolport-gtk` binary with no impact on existing build paths;
- responsive application window with no forced geometry;
- read-only Omarchy palette parser, GTK CSS generator, and live reload;
- fallback theme for non-Omarchy Linux sessions;
- tests for palette parsing, path resolution, invalid input, and CSS safety;
- CI build and test command for the opt-in shell; and
- a recorded Hyprland smoke result for tiling, resize, move, fullscreen,
  floating, scaling, and theme reload.

Exit gate: current Toolport checks pass, the new binary runs natively on
Wayland, and no existing package includes it by accident.

Current implementation floor: GTK 4.10 and libadwaita 1.4. This supports the
adaptive navigation primitives used by the preview and is available on current
Omarchy. The dedicated CI job uses Ubuntu 24.04 so the existing Ubuntu 22.04
Tauri job and its older desktop libraries remain untouched.

Current checkpoint:

- the native window uses the active Omarchy palette, translucent layered
  surfaces, adaptive navigation, and compositor-owned geometry;
- the preview reads real server names, transport types, enabled state, profile
  count, and the active profile from the existing registry;
- startup calls Toolport's operational loader once so recovery and safety checks
  match the shipping shell, while later display refreshes remain parse-only;
- environment values, credentials, and keychain data never enter the native
  display model;
- a file monitor refreshes the display after another Toolport process replaces
  the registry; and
- missing, unreadable, and invalid registries have separate non-destructive UI
  states instead of silently appearing as an empty configuration.

Server enable, disable, add, edit, and remove use a tested shell-neutral
controller, and both the Tauri and GTK shells call the shared mutation paths.
GTK runs locked writes away from the UI thread, then reconciles from the
committed registry or restores its last good snapshot on failure. Native edits
cover non-secret connection fields and preserve existing environment
declarations, secret references, tool settings, team provenance, and unknown
newer fields. Environment credentials can be added, replaced, and removed
through the same cross-process keychain transaction used by Tauri. The native
UI sees declared key names but never reads stored values back, and a registry
failure restores the previous keychain value when possible. OAuth client-secret
configuration, manual bearer auth, and client-credentials auth all use shared
controller transactions. The GTK view can replace or remove secrets but never
reads them back. Do not add direct GTK write paths to the registry as a shortcut.

Server cards and the add/edit form can now run bounded connection tests without
persisting unsaved form values. The probe implementation is shared with Tauri,
injects only declared vaulted secrets, classifies authentication failures, and
runs away from the GTK thread. Remote server cards also expose manual bearer
authentication. The status check never reads a token into the view, replacement
clears stale OAuth refresh metadata first, removal is serialized with other auth
mutations, and both shells share the same keychain and gateway-reload behavior.
Browser OAuth now shares Tauri's cross-process flow ownership, atomic completion
receipt, refresh-state ordering, and keychain writes.

Native server search now filters by name or transport and is available through
Ctrl+F. Add, edit, and credential windows use the same Omarchy-derived surface,
input, focus, feedback, and action styles as the main shell. Their content is
clamped instead of stretched, grouped by task, scrollable in short tiles, and
keeps secret replacement controls usable at narrow widths.

The native sidebar now switches real pages, scrolls in short tiles, and exposes Ctrl+1 through Ctrl+9
navigation. Clients has a read-only local scan, installed/connected/configured
summary, ownership-aware gateway status, and manual refresh. Its display model
keeps counts and status only: config paths, commands, arguments, environment key
names, and parser details do not enter the GTK view. Native connect, reset, and
disconnect now use the same client-file writers and managed-entry bookkeeping as
Tauri. A customized Toolport entry requires explicit confirmation. If the
registry write fails, Toolport restores the client backup only when no newer
external write has replaced the file. Clients can connect through stdio or the
supervised Shared HTTP endpoint, securely revoke Shared HTTP bearers on
disconnect, and bind an existing connection to one profile or follow the active
profile. Profile create, switch, and delete are available from Servers.
Profile actions can enable every reviewed server or disable the whole profile,
and each server can narrow the active profile to an explicit tool allow-list
after loading the server's advertised catalog.
Clients can also review servers discovered in installed client configurations
and import only selected entries. The shared dedupe path excludes Toolport's own
gateway and leaves every imported server disabled for review.

Activity now reads the retained local audit log off the GTK thread, shows total
calls, success rate, average latency, and the most recent 100 tool calls, and can
clear the call audit, discovery traces, live inspector ring, and savings tally
after confirmation. It also shows recent security drift, injection events,
lazy-discovery searches, and opt-in live inspector captures. The ordinary audit
view model accepts server, tool, client label, outcome, timing, and the
already-bounded error message only. Tool arguments and results enter only the
explicit live inspector section after capture has been enabled in Settings.

Rules now uses the existing shell-neutral rules engine for rule-set creation,
editing, selection, deletion, per-client coverage, and project registrations.
Enabling a client or project file opens the engine's exact dry-run output before
writing. Project folders expose only supported files, keep file-level consent,
and use explicit review before apply. Imports open as unsaved drafts for review.
Saves, active-set changes, opt-outs, project cleanup, and imports retain the
existing drift detection, cross-process registry lock, and owned-block cleanup
behavior.

Settings now shares the registry's existing safety setters for destructive-tool
blocking, agent confirmation, human approval, lazy discovery, Code Mode,
routine writes, agent control, and live inspection, plus the gateway's
effective team-enforced state for content defense, injection blocking, drift
quarantine, and PII pseudonymization. Turning live inspection off also clears
its ephemeral capture buffer. Team-forced switches are visible but locked. The
native shell also reads and updates the same Linux launch-at-login entry as the
shipping app. Settings can start and stop the shared supervised HTTP endpoint,
shows and copies its authenticated local URL, exposes the administrator token
only through an explicit clipboard action, restores an enabled endpoint on launch, and
can mint one-time bearer tokens for named HTTP clients scoped to all servers or
a selected profile. Only token hashes are retained, and revocation does not
silently remove the managed token for a connected desktop client. The desktop
section can stop gateways left behind by an upgrade, preserves the current HTTP
endpoint, and names client apps that need restarting. It explains that native
updates come through Omarchy or pacman and never self-install. It also lists
quarantined tools with an explicit review and re-approval flow. It
lists fingerprint-bound session and persistent approval exceptions, can revoke
them, and surfaces value-free routine suggestions with dependency, provenance,
risk, and source review before immutable persistence.

Per-client Full, Lazy, and Grouped discovery overrides are available from each
connected client card and inherit the global mode by default. Project folders
can be mapped to server profiles from Settings, using the gateway's existing
longest-prefix routing behavior. Lazy-discovery prerequisites can be pinned in
Playground and reviewed or unpinned together from Settings.

Catalog now browses the bundled curated set, searches the official MCP Registry,
detects servers already present, and adds individual servers or starter stacks
disabled by default. Playground uses one shell-neutral controller shared by GTK
and Tauri to inspect tools, resources, and prompts, invoke them with JSON
arguments, display raw results, and record tool calls in Activity. Teams can join
direct or approval-gated invites, sync, leave, and explicitly review local-command
or private-address servers before enabling them. Team administrators can compare
their current local set with the shared set and apply the exact reviewed update
through the existing version- and fingerprint-bound push transaction.

A native first-run assistant now offers separate MCP and agent-rules paths,
starter stacks, reviewed client imports, direct client connection, a Teams
handoff, and an enabled-server health check. It appears only for a genuinely
fresh setup, records dismissal outside the registry, and remains available from
the application menu for replay. The batch health selection and probe runtime
are shared with the shipping shell rather than duplicated in GTK.

Servers can export a secret-free setup file and import a local setup after an
exact launch-target review. The export path strips environment values, masks
inline credential arguments, removes URL userinfo, and excludes Toolport's own
gateway. Imports dedupe by server name and leave new entries disabled.
The native shell can also create an expiring share link after confirmation and
copy it to the clipboard. Packaged `toolport:` and legacy `conduit:` links open
the same review flow through the desktop URL handlers, including on cold start.

Settings can copy the same secret-safe diagnostics bundle as the shipping shell
and open Toolport's local data directory. Activity exports the retained audit
log as formula-safe CSV or JSON without adding live inspector request or result
bodies.

Agent permissions now uses the existing Claude Code policy and Cursor guard
engines. It supports rule creation and removal, presets, per-profile status,
exact settings previews, enforcement opt-in, and Cursor Off, Observe, and
Enforce modes. Agent activity uses the existing non-blocking hook sensor for
opt-in, per-profile status, exact hook previews, and the recent content-free
event log. All file mutations retain the engines' authoritative registry lock,
ownership records, validation, and fail-closed behavior.

The approval broker now accepts shell-neutral host callbacks while the Tauri
adapter preserves its existing events, notifications, and registry allow checks.
The native shell owns the same authenticated, fail-closed broker lifecycle and
shows pending calls with their exact deadline, reason, arguments, URL action, and
PII release disclosure. It supports one-time approve and deny decisions plus
session and persistent fingerprint-bound approval scopes. An exclusive owner lock and live-endpoint probe prevent two
Toolport shells from replacing each other's broker socket or descriptor.

Closing the native window now hides it while its registry and theme monitors,
approval polling, and broker remain active. Launch-at-login starts hidden without
mapping a window. A second launch and the StatusNotifierItem tray present the
same window instead of creating another process or broker. The tray menu,
application menu, and Ctrl+Q provide explicit final quit paths that remove only
an endpoint owned by that process. New pending approvals send one desktop
notification with a non-sensitive summary, and the notification is withdrawn
when the call resolves. The tray carries a bundled icon and remains usable even
before the desktop icon theme has Toolport installed.

Debug builds retain Toolport's existing `Toolport-dev` data-directory isolation.
An intentional check against installed data can use Toolport's existing
`TOOLPORT_DATA_DIR` override for that launch only. The native app must not
silently fall back from development data to installed data.

An opt-in native staging script now builds optimized GTK and gateway binaries,
lays out a Linux root payload with desktop entry, AppStream metadata, icons, and
license, then validates the metadata. It does not change the shipping Tauri
bundle or release workflow.

An additive Arch `PKGBUILD` preview consumes a tagged source archive, performs a
locked release build and native tests, and installs the same staged metadata and
runtime payload. Its checksum remains `SKIP` until a real release artifact is
selected, so it is a maintainer preview rather than a shippable package.

`scripts/test-linux-native-package-lifecycle.sh` builds the staged payload into
local Arch packages, then uses an isolated pacman root to exercise install,
upgrade, rollback, and uninstall. Synthetic Toolport registry and client config
fixtures are hashed after every transaction, and package-owned payload changes
are checked in both directions. The staged package includes the Agent Plugins
archive under `/usr/share/toolport/agent-plugin/` and verifies its manifest and
skill payload. A real clean-chroot package build and signed release checksum
remain release gates.

### Phase 2: shared controller and background lifecycle

Deliverables:

- shell-neutral registry state and mutation controller;
- shell-neutral event sink replacing direct Tauri handles where necessary;
- approval broker lifecycle and pending/resolved events;
- registry file watcher and external-change refresh;
- autostart and URL opening; and
- StatusNotifierItem tray and desktop notifications verified in Omarchy.

Exit gate: an approval raised by an existing gateway appears in the GTK shell,
can be decided, and behaves identically to the Tauri path.

### Phase 3: daily-driver vertical slice

Deliverables:

- Servers list, search, enable, add, edit, remove, test, OAuth, and secrets;
- Clients detection, detail, connect, disconnect, migration, and profiles;
- Pending approvals and quarantine alert;
- essential security, discovery, HTTP endpoint, theme, and autostart settings;
- minimal first-run onboarding; and
- keyboard navigation and accessibility names for every control.

Exit gate: a fresh Omarchy user can configure servers, connect a client, use
Toolport with approvals enabled, and recover from expected errors without the
Tauri shell.

### Phase 4: governance and security parity

Deliverables:

- Rules and project rules;
- agent permissions and guard mode;
- hooks;
- quarantine management and tool overrides;
- team connection, sync, review, and push; and
- shared controller tests derived from current React behavior tests.

Exit gate: all security-sensitive mutations and fail-closed behavior have
equivalent tests for both shells.

### Phase 5: remaining product parity

Deliverables:

- Activity, audit statistics, security events, traces, inspect, and diagnostics;
- Catalog and stacks;
- Playground resources, prompts, tools, results, and schema-driven arguments;
- import, export, sharing, deep links, and file dialogs; and
- remaining onboarding and low-priority presentation state.

Exit gate: the command parity inventory has no unexplained gaps. Features may
be intentionally removed only through a documented product decision.

### Phase 6: packaging and release candidate

Deliverables:

- one Arch package containing `toolport` and `toolport-gateway`;
- desktop entry, icons, AppStream metadata, URL handlers, and license;
- direct runtime dependencies on GTK4, libadwaita, libsecret, and required
  desktop integration libraries;
- clean-chroot source build;
- Linux updater behavior replaced with package-manager guidance;
- migration of durable WebKit preferences that still matter; and
- release artifacts that retain the Tauri Linux build as a rollback option.

Exit gate: install, upgrade, downgrade, and uninstall preserve user registry,
secrets, client configuration, and gateway availability.

### Phase 7: beta, replacement, and Omarchy proposal

Run a beta on current Omarchy stable and edge, plus GNOME Wayland, KDE Wayland,
and one X11 session. Track crashes, approval reliability, keyring prompts,
fractional scaling, long-running background behavior, and resource use.

Replace the default Linux shell only when:

- the beta has no unresolved data-loss, security, approval, startup, or client
  configuration regressions;
- the functional parity inventory is complete;
- the package has a tested rollback path;
- Windows and macOS release jobs are still green; and
- the native shell has completed a real-world soak across at least two
  Toolport releases.

After replacement, prepare the source package and concise inclusion proposal
for the Omarchy package repository. Core-image inclusion is an upstream product
decision, not a technical completion criterion.

## Verification matrix

### Automated on every change

- Rust formatting and clippy for affected feature sets.
- Existing Rust tests with default features.
- Existing Rust tests with no default features.
- GTK module unit tests with `gtk-desktop` enabled.
- Existing frontend type-check, lint, and Vitest suite.
- Headless gateway build and smoke test.
- Windows and macOS headless Rust tests.

### Linux desktop behavior

- Hyprland native Wayland at 1x, fractional, and 2x scale.
- Narrow, wide, short, stacked, fullscreen, and floating layouts.
- Theme switch while the application is visible and while it is backgrounded.
- Tray host absent and present.
- Notification daemon absent and present.
- Locked and unlocked Secret Service.
- Launch normally, launch hidden, second launch, deep link, close, and quit.
- Registry changed by another Toolport process while a view is open.

### Regression-sensitive workflows

- Add, edit, enable, disable, and remove a local or remote server.
- Store, update, use, and remove secrets without exposing plaintext.
- Connect, migrate, and disconnect every supported client fixture.
- Approve, deny, timeout, and lose broker connectivity.
- Import and export without overwriting unrelated configuration.
- Upgrade and rollback without changing registry or client paths.

## Working estimate

With focused AI-assisted implementation:

- Phase 1: two to four days;
- Phase 2: four to seven days;
- Phase 3: two to three weeks;
- Phases 4 and 5: three to five weeks; and
- Phases 6 and 7: one to three weeks plus soak time.

The first useful beta should arrive well before full parity. Regression gates,
not generated widget volume, determine when it is safe to replace the current
Linux application.
