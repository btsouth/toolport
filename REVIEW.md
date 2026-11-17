# Review checklist

Each entry below comes from a defect that reached Toolport at least twice. Read the diff, match it against every Trigger, and check only the entries whose trigger matches. Name the entry in each finding (for example "Keyring prompts: ...").

Tag every finding:

- Important: the diff adds or keeps a bug a user would hit. Blocks the change.
- Nit: style, wording or small cleanup. Never blocks.
- Pre-existing: the bug was already there before this diff. Report it, but it does not block the change.

Reviewers read this file from the base branch, so a pull request cannot change the rules that judge it. Changes to REVIEW.md land in their own pull request.

## Both shells (React and GTK)

### GTK and React parity

- Trigger: a change under `src/components/`, `src/App.tsx` or `src/lib/` that adds or changes something a user sees or does, with no matching change under `src-tauri/src/linux_native/`; or the reverse.
- Check: the other shell gets the same feature at the same design and quality in this PR (same controls, states, wording, counts, limits, warnings and empty or error screens). A simplified GTK version never ships, and "GTK follow-up later" is not acceptable unless the PR is explicitly React-only by design. Compare numbers both shells show (row limits, counts, how entries are counted).
- Why: GTK hid cleanup warnings React showed (#1390); GTK showed 100 Activity rows and React 200, and GTK had no appearance setting; recovery cards differed until #1355; Activity counts in #1366 and #1367; Skills fixes needed in both apps (#1218, #1250, #1256).

### One shared Rust path for shell behavior

- Trigger: logic that decides an outcome (connect, disconnect, import, counting, matching, credential handling) added in a React component or in `linux_native/` instead of in a shared Rust module such as `registry_controller.rs`, `clients/` or `desktop.rs`.
- Check: both shells call the same Rust function for the decision; neither shell reimplements it. If one shell already had its own copy, the PR routes it through the shared path rather than patching both copies.
- Why: React Disconnect took its own path until #1356; the shells counted a Connect all result differently until the rescan decided for both (#1390); a GTK Add path skipped the shared import preparation and wrote secret arguments to `registry.json` in plain text (found in review of the GTK credentials work).

### Narrow windows

- Trigger: a new or changed row, header, toolbar, chip, dialog, dropdown or menu in `src-tauri/src/linux_native/` or `src/components/`, or CSS width, min-width or no-wrap changes.
- Check: at 480 px wide (and around the 1000 px rail breakpoint) nothing runs past the window edge: long server, tool and client names wrap or ellipsize with the full text reachable, status chips keep whole words on one line, actions move below the name instead of widening the page, and popups fit inside the window.
- Why: #1098, #1256, #1357 (row menu off the window), #1369, #1380, #1392, plus the narrow window fixes in the GTK baseline audit.

### GTK popups and tooltips

- Trigger: a new `MenuButton`, `PopoverMenu`, `Popover`, `DropDown`, split button, suggestion list or any `popup()` call under `src-tauri/src/linux_native/`.
- Check: it goes through `quiet_tooltips::guard_menu`, `guard_dropdown` or `guard_popover`, and the source check covering every popup still passes. A popup opened over a showing tooltip freezes the whole app on Hyprland.
- Why: #1344 fixed menus, then dropdowns, the Skills Open with menu and Catalog suggestions froze the same way (#1350).

### GTK main loop stays free

- Trigger: code under `src-tauri/src/linux_native/` that reads or writes the registry, keyring, team server, network, package manager or large files, or takes a blocking lock, inside a signal handler, render or build function.
- Check: the work runs on a worker thread and results come back through the main context, with a generation check so a late result cannot overwrite newer state. Keyring calls in particular never run from render, because an unlock prompt then freezes the window.
- Why: #1171 moved registry and team reads off the main loop; #1376 fixed a window freeze under a keyring prompt when a recovery card rendered; #999 stopped search and focus from probing servers.

### Page state and late async results

- Trigger: React state added to a list view that navigates to a pushed detail page (`ClientsView`, `ServersView` and similar), or an async call whose result updates a dialog or step that can close first.
- Check: state that must survive opening details and coming back lives with the app's page data (`App.tsx`), not in a component that unmounts. A result or prompt answer that arrives after its dialog or step closed is ignored. Coming back from details rescans when the data could have changed.
- Why: `ClientsView` lost T3 Code instances on return from details (#1347) and lost the Connect all summary (#1390); a keyring answer after the step closed was applied (#1390); rescan after details added in #1358.

### User-visible text and translations

- Trigger: a new or changed user-visible string in `src/` or `src-tauri/src/linux_native/`, a new number shown to users, or edits under `src/locales/`.
- Check: the string goes through i18n in both shells with keys in every locale file (`npm run check:locales` passes; no hidden or zero-width characters). GTK text set when a widget is built is rebuilt on language change (`i18n::on_language_change`), including accessible descriptions, tab counts and window titles. Numbers use locale digit grouping in GTK (`i18n::group_digits`) as React does with `toLocaleString`.
- Why: untranslated GTK text (#1188, #1201); zero-width spaces in translations (#1291); a GTK button description kept the old language (#1358); GTK Activity counts missed digit grouping (#1367).

## Secrets and keyring

### Keyring prompts

- Trigger: a new call that reads secrets (`secrets::get_secret`, `get_secret_result`, vault or backup reads) or a call path into one, especially from startup, a refresh, a health probe, a background check, the Clients page or a batch over several servers or clients.
- Check: code the user did not start reads without prompting (`secrets::without_prompts`, `get_secret_without_prompt`, `store_would_prompt`) and shows a locked state instead of asking. A user action asks at most once for its whole run (`secrets::asking_once`), even across several reads or steps. A refused unlock is detected from the store's real refusal, not from a call count. Cancel leaves files and the registry unchanged.
- Why: start-up unlock prompts (#1368); prompts from the Clients page and start-up client refresh (#1376); Disconnect asking repeatedly (#1378); Connect all asking twice and counting every read as a prompt (#1394).

### Secret values never leave in plain text

- Trigger: a change to logging, error messages, tool results, relayed stderr, share or export output, Activity records, or code that writes server definitions (`registry.json`, client configs, shared setups).
- Check: stored secret values are redacted on every path the change touches, including error strings and relayed child output, and redaction does not rewrite protocol fields such as a JSON-RPC `id`. Secrets entered as arguments, env values or URLs are moved to the keychain or a reference before anything is written to disk. Messages about a refused paste never quote the pasted text.
- Why: #781, #1001 (share URL credentials), #1343 (stored secrets in logs, errors and results; review caught a redacted reply `id`); inline argument secrets in #1366.

### Imported or shared configuration runs nothing without consent

- Trigger: a change to import, shared setup, team, sync or paste flows (`sharing_controller.rs`, `teams.rs`, `personal_sync.rs`, `clients/claude_add.rs`, import review dialogs).
- Check: imported servers arrive switched off as the dialog says; no local command starts and no remote server is contacted until the user turns it on. Team or shared items cannot enable themselves past a review.
- Why: shared setup import started local commands and contacted remotes at once (#1348); team review enables needed a guard (#1001).

## Client configs

### Identity is not a name

- Trigger: code that matches servers, client config entries, gateway entries or import candidates by `name` or key alone (`==` on a name, a map or set keyed by name, `BTreeSet` of names).
- Check: identity uses the full rule (transport plus command and args, or URL; for Toolport's own entry, the whole entry it wrote and its token). Two different servers with one name stay distinct, and a user's customized entry under Toolport's name is never treated as Toolport's to rewrite.
- Why: a user-customized `toolport` entry was rewritten on every launch (#488); a v1 Shared HTTP entry had to be proven by its whole content and token (#1370); import and Review and connect matched by name and created duplicates (found in the GTK baseline audit); a macOS acceptance check accepted any entry named `filesystem` (#1339).

### Preserve what the user wrote in client configs

- Trigger: a change under `src-tauri/src/clients/` or `src-tauri/src/clients.rs` that writes, rewrites, moves or restores a client config (JSON, JSONC, TOML, YAML).
- Check: comments, ordering, unrelated entries and user edits survive the write and the later restore, including JSONC comments and TOML boundary comments on child tables such as `[mcp_servers.x.env]`. A test covers the round trip: connect, user edit, disconnect or restore.
- Why: JSONC comments lost (#592); TOML boundary comments lost (#1315, with five more review findings on restore ordering); customized entries reverted (#488); original configs restored (#1063, #1067).

### Client config file resolution

- Trigger: code that builds or compares a client config path, a hard-coded config filename, or a path shown to users.
- Check: it uses the client's resolver (for OpenCode, `opencode.jsonc` as well as `opencode.json`, and an explicit `OPENCODE_CONFIG` used exactly as named) and compares against the same file the label and the writer use. Shown paths are joined the same way on every platform, and Windows drive-rooted paths are handled.
- Why: OpenCode jsonc missed (#827, #1347, #1358, #1365); explicit `OPENCODE_CONFIG` replaced (#1340); Windows path display and drive-rooted paths (#1365).

### Disconnect removes every form Toolport wrote

- Trigger: a change to disconnect, uninstall, purge, restore or relocation code (`clients/restore.rs`, `clients/relocate*`, `uninstall_gateway`, `desktop.rs` disconnect paths).
- Check: every entry shape any Toolport version wrote is found and removed (v1 Shared HTTP, `npx mcp-remote` bridge, moved native servers), servers Toolport moved are put back, keychain cleanup can be retried, and restoration still works when the registry is damaged or unreadable.
- Why: old HTTP entry survived Disconnect (#1340); v1 Shared HTTP entry after a 2.x connect (#1370); moved and imported servers restored on Disconnect (#1002); keychain cleanup could not be retried (#1356).

### Rollback covers every write

- Trigger: a multi-step operation that writes more than one file, the registry and the keychain, or that has a rollback or undo branch.
- Check: every write made before the failing step is in the backup or undo set, including old config files and keychain values; a rollback never hides values it could not restore and reports them; concurrent edits made during the operation are kept.
- Why: writes to old config files were not in the rollback (#1340); rollback hid missing values (#1342); rollback notes needed to stay readable after the keyring change (#1390).

## Gateway and processes

### Every dispatch checks live policy

- Trigger: a new or changed path in `src-tauri/src/bin/toolport-gateway.rs`, `router.rs`, `downstream.rs` or `stdio_adapter.rs` that calls a downstream server, starts a server or changes what a session can see.
- Check: the call goes through `Router::authorize()` against the live policy (rechecked if the request started on an older router), turned-off and out-of-scope servers never start, quarantine and alias checks match exactly, and policy or visibility changes notify every affected session, including rooted (`${ROOT}`) ones.
- Why: #1012 (dispatches not all authorized); #1126 (policy refusals); #1351 (cold Run had to check policy before and after start, exact alias match); #1320 review (rooted sessions not notified).

### Spawned and reaped processes

- Trigger: a new `Command::spawn`, opener or launcher call, or code that signals, kills or reaps processes by pid or name.
- Check: launchers that may exit at once use `hostenv::spawn_reaped` (or are otherwise waited on) so no zombie is left. Kill and reap code is scoped to its own data directory and build and proves the pid is still its own process before signalling, since pids are reused.
- Why: zombie `xdg-open` per opened link (#1346), same for file openers (#1353); a dev build reaped production gateways (2b47a6924) and reaping was scoped to the data dir (#952); Windows test cleanup killed reused pids (#1314).

## Tests and checks

### Test isolation

- Trigger: a new or changed Rust test that touches env vars, the data directory, logs, the keychain or credential store, or child processes.
- Check: it holds `registry::DataDirOverride` or the data-dir test lock and the existing `ENV_LOCK` or override helper instead of setting process-global variables, never points at the user's real data directory, and selects its own rows (unique client or id) rather than assuming it is the only writer.
- Why: #1015 (test cleared a shared env var mid-flight), #1034 (savings row from another test), #1321 review (recovery tests without isolated data dirs), #1331 and #1332 (Windows credential tests overlapping).

### Timing in tests

- Trigger: a new test with a fixed `sleep`, a short timeout or deadline, or one that measures elapsed time.
- Check: it waits on a condition with a generous deadline, does setup (encoding, catalog build, process start) before timing starts, and reports elapsed time on failure. Existing timeouts are not loosened just to get a green run without explaining the cause.
- Why: load-sensitive flakes fixed in #1005, #1015, #1070, #1090, #1210 and #1331.

### Tests that prove the fix

- Trigger: a PR that claims to fix a bug, or adds a GTK widget test, source-scan test, acceptance script or CI check.
- Check: the test fails without the fix and drives the real trigger (shows the tooltip before the menu, presses the real key in the focused widget, starts Test or Save). Scanners really cover the files they claim. Check scripts treat a failed or timed-out probe as a failure, never as a pass.
- Why: tooltip CI test never showed a tooltip (#1344 review); Escape test emitted signals instead of key presses and the popup scanner skipped production code (#1354); busy-button test never started Test or Save (#1357 review); acceptance probes that passed on failure (#1339 review).

## Release and packaging

### macOS disk image steps

- Trigger: a change to release scripts or workflows that call `hdiutil` (create, convert, attach, detach) or build the dmg.
- Check: each `hdiutil` step retries a briefly busy volume with a bound, and the image contents exclude system files such as the file-event journal.
- Why: #1352 (detach busy), #1391 (create and convert busy), #1389 (file-event journal in the dmg).
