# Uninstall

Toolport installs a gateway entry into each AI client's own config file and keeps
its state in a per-user data directory. A clean removal has two parts: disconnect
the clients while Toolport is still installed, then remove the app. Normal uninstall keeps your registry and credentials. Verified client recovery releases backup payloads; upgrades keep connections and recovery data.

## 1. Disconnect your clients

In **Settings**, choose **Remove Toolport from all clients** while Toolport is
still installed. Review the per-client results and resolve any errors before
removing the app or its data. You can also run `toolport-gateway --disconnect-all`
as your normal user; `--dry-run` lists the affected clients without changing them.
Do not run this command with sudo. Untouched configs return to their exact original
bytes; subsequent unrelated edits are preserved or reported as conflicts.

Removing Toolport from all clients also stops the Toolport gateway daemon unless a
client is connected to it right now. A connected client keeps working until it
disconnects; the daemon then exits at once instead of waiting out its idle time.
The same happens when you quit Toolport. A daemon whose app was deleted, moved to
the Trash or replaced by a package upgrade notices within a few seconds and exits
as soon as no client is connected.

Backups of imported credentials are stored in the vault with a recovery copy
that omits secret values. At desktop startup, Toolport also seals older plaintext
backups before updating client gateway paths. If that migration cannot use the
vault, it retains the original backup and reports the failure; unlock the vault
and restart Toolport to retry. If a saved value cannot be read, restoration still recovers the config and
keeps a placeholder for that value. **Clients** lists each server and key with
its reason and next step. A locked keyring needs unlocking; an unavailable store
needs starting or reconnecting; duplicate matches need resolving. A missing or
damaged saved value can be re-entered in the client, or entered in Toolport before
choosing **Retry**. Access rejection and cancellation have their own messages.
Technical placeholder names appear only in **Details**.

Retry restores unchanged placeholder leaves, including entries you moved, while
preserving your edits, comments and ordering. A value you replaced yourself or a
server entry you deleted is accounted for without being overwritten or restored.
Results state how many values were restored and how many still need attention.
The receipt and saved values are retained until the committed file has no owed
placeholders. Partial recovery never removes its receipt. Recovery also works
from `toolport-gateway --disconnect-all`, even for an already disconnected client.

Pending notices survive restarting either shell. **Dismiss** only hides a notice;
**Show recovery** brings it back. The notice and confirmation show the backup path
and explain that it does not expire automatically. **Discard these saved values**
is separate and permanent: its confirmation lists the saved values and every
backup file it will delete, including older snapshots and the matching move
record. The current client config and server credentials do not change; temporary
text for missing values stays until you replace it yourself. If deletion starts
but cannot finish, the notice says which material may already be gone and offers
**Finish discarding** with a fresh list of remaining files. Restoration is blocked
for that receipt once deletion has started. **Scan clients again** and Ctrl+R
check saved-value availability without an unlock prompt or changing the client
config; available values are marked ready for **Retry**. Malformed or untrusted receipts are
listed separately with a repair reason and cannot authorize unsafe writes or
deletion. One bad receipt does not hide other recoveries.

Linux backup reads and deletion never open keyring unlock prompts. macOS backup
queries use `kSecUseAuthenticationUIFail`: interaction-not-allowed, authentication
failure and cancellation map to separate states. Credential Manager maps reported
Windows access, authentication, cancellation, missing and corrupt-data errors.
Windows has no separately lockable generic-credential vault, so normal reads do
not produce a keyring-locked state or an unlock prompt to cancel. Its unique
credential target names also do not produce Secret Service's duplicate-match
state. Keychain's service/account/access-group query does not report that Linux
match-cardinality state either. Payload integrity failures are checked by
Toolport on every platform.

Status mapping and backup round-trip tests run in **Headless Rust tests
(macos-latest, core)** and **Headless Rust tests (windows-latest, core)** in
`.github/workflows/ci.yml`. Unsigned macOS runners assert the production Keychain
entitlement refusal instead of publishing an unprotected backup; real Keychain
round-trip acceptance needs the signed app with its provisioning profile.

Data removal refuses to discard incomplete recovery.
After verified restoration, historical backup payloads are deleted and only
empty ownership metadata remains, so a later uninstall cannot claim native entries.
Turn off **Launch at login** in Settings.

Windows NSIS and native GTK deb/rpm/Arch packages attempt the same operation before
removing their gateway (`--disconnect-all --uninstall`), which also removes
Toolport's own launch-at-login entry; a changed or unrecognized entry is kept and
named. Failure is logged with recovery instructions and removal
continues. Keep Toolport's data on failure, reinstall it, and retry the action above.
Linux hooks enumerate accounts with `~/.config/Toolport` or `~/.config/Conduit`
and switch to each account without a login shell. For a custom `XDG_CONFIG_HOME`
or `TOOLPORT_DATA_DIR`, use the in-app action or CLI with that environment first.
Upgrades skip the removal action. Windows installers defer while client gateways
are open and name the affected clients: close their MCP sessions and retry, or
cancel to install later. They never force-kill gateways by name.

Windows uninstalls then stop any gateway still running from the install folder,
because Windows cannot remove a running program; a client that was connected
loses its gateway at that point.

**Homebrew:** the cask in the `btsouth/homebrew-toolport` tap runs the same
operation on `brew uninstall --cask toolport`, while the app is still installed,
then quits Toolport. Upgrades and reinstalls skip it, so clients stay connected.
The cask checks both Homebrew's call stack and the brew command; if they
disagree it leaves every client connected and prints the command to run by
hand. If it cannot restore a client it prints a warning and the uninstall continues;
Toolport's data, which holds the original configs, is kept.

**AppImage, Tauri .deb, and dragging macOS to the Trash:** use the in-app action
and turn off **Launch at login** in Settings before deleting the app. These have
no uninstall hook, so nothing can remove the login entry after the app is gone. The native GTK .deb is
distinct from the Tauri .deb. On macOS the CLI is
`"/Applications/Toolport.app/Contents/MacOS/toolport-gateway" --disconnect-all`
(adjust the app path if needed).

## 2. Remove the app

- **Windows:** Settings > Apps > Installed apps > Toolport > Uninstall, or run
  `winget uninstall Toolport.Toolport`. The NSIS uninstaller removes the app and its
  bundled gateway.
- **macOS:** quit Toolport, then drag `Toolport.app` from Applications to the Trash.
  With the Homebrew cask, run `brew uninstall --cask toolport`. It keeps Toolport's
  data and credentials; `brew uninstall --cask --zap toolport` removes them too
  (see below).
- **Linux:** use the package manager that installed it:
  - pacman repository: `sudo pacman -R toolport`. See
    [Arch and Omarchy](arch-pacman-repo.md) for removing the repo and key.
  - `.deb`: `sudo apt remove toolport` (or `sudo dpkg -r toolport`).
  - Native GTK `.rpm`: `sudo dnf remove toolport`.
  - AppImage: disconnect clients, quit Toolport, then run the installer cleanup as
    your normal user: `bash install.sh --uninstall-appimage` (download
    [install.sh](https://toolport.app/install.sh) first). This removes the actual
    extensionless `$XDG_BIN_HOME/toolport` (default `~/.local/bin/toolport`),
    desktop launcher, icons, autostart entries and update resume state.
    Use the same XDG paths as installation. For a manually downloaded AppImage,
    also delete that file.

## Optional: remove Toolport data

While Toolport is installed, choose **Remove Toolport data** in Settings. The
confirmation lists the data directory, credentials and startup entries affected,
and the report file in your home directory. Toolport closes before removal.
Retrying the same confirmation keeps earlier reports beside that file with a
`.previous-` suffix and writes the new result to the listed report path.
This is permanent and never runs during an upgrade or normal uninstall.

For the CLI, close Toolport and run as your normal user, without sudo:

```sh
toolport-gateway --remove-data --dry-run
toolport-gateway --remove-data --confirm
```

On macOS, use the gateway in `Toolport.app/Contents/MacOS/` as in step 1.
The CLI prints JSON results. Exit 0 means complete, 1 means leftovers or a
failure, and 2 means confirmation was required. Review the report before
removing the app.

Removal first inventories startup entries and checks sessions without changing clients.
It restores and disconnects clients through the same path as step 1 only after
that preflight passes, then re-checks sessions before deleting credentials or data.
A failed restoration or retained edited Toolport entry keeps recovery data.
Resolve the reported client paths before retrying. Active gateway sessions block
removal: close the listed sessions and retry. Toolport requests a graceful stop
only from authenticated daemons for this data directory.

The action removes Toolport-created entries from the selected data directory,
including registry, logs, caches, migration exports, client backups, encrypted
secrets and published gateway copies in `bin/`. Other files and folders are
preserved and listed in the report, including unrelated entries inside Toolport's
folders. Generated files are matched by their exact names or naming patterns;
a folder is removed only when empty. The data directory may remain.
Settings and the default CLI action remove only the selected registry's server
accounts, excluding ids owned by detected other installs. Toolport's shared master key,
internal keys, HTTP bridge/client keys and Team tokens are preserved, along with
unknown accounts that may belong to another Toolport install. Settings stays scoped; the CLI option
below provides explicit consent to remove shared credentials. Accounts have no
install id, so an undiscovered install using the same server id shares that token.
Unreadable ownership in another detected install blocks credential cleanup and
keeps the selected registry for retry. The Settings plan and CLI report name the unreadable path before cleanup.
Folders without a registry or registry recovery files are not treated as installs;
preserved user files from an earlier removal do not block a later removal.
Removed Team tokens are deleted locally only; the Teams client has no server-side
revoke endpoint.

To explicitly remove **all installs' credentials** in the shared `conduit-mcp`
service, including orphaned chunks and master keys, use the separate CLI opt-in:

```sh
toolport-gateway --remove-data --all-installs --dry-run
toolport-gateway --remove-data --all-installs --confirm
```

For a custom `TOOLPORT_DATA_DIR` or `CONDUIT_DATA_DIR`, choose `--all-installs`
before removing its registry. Afterward, the command cannot verify ownership of
that directory. Remove the preserved `conduit-mcp` accounts in your OS credential
manager instead.

This clears credentials used by other installs too, while deleting only
Toolport-created entries in the selected data folder. It can proceed with a
corrupt registry, using independent client recovery snapshots. Client restoration
failures still retain recovery data. Settings uses the automatic scope above.
On a truly headless Linux host with no session bus or runtime directory, removal
continues without a keyring. An active encrypted file backend also permits
headless cleanup when no Secret Service is available. On a desktop, Toolport first
tries the runtime directory's bus and inventories older keyring tokens even if
the file backend is selected. An unavailable desktop bus or Secret Service
provider reports credential cleanup as incomplete and keeps the registry so
retry can find the same accounts. Startup cleanup precedes
credential deletion. Each credential is removed on its own: one that cannot be
removed never stops the others, and the rest of the data is still removed. The
report lists what was removed, what was already gone (`absent`, for example a
legacy keychain view of an item that was just deleted), and every leftover with
its reason. When a credential is left over, `registry.json` and
`client-locations.json` are kept, and listed, so a retry finds the same accounts,
and the command exits with status 1.
Toolport's own launch-at-login entries are removed; changed or unrecognized
entries are preserved and reported. Failed removals list the exact resource
paths, or the credential scope if the OS denied inventory access.

The resource inventory also includes app caches, preferences, saved state and
WebView storage for `com.tsout.conduit`. These are removed for a sole default
install and preserved with an explanation when another install or a custom data
directory is in use. Custom data directories require a valid
Toolport registry regardless of their folder name, except for the explicit
`--all-installs` option when a corrupt registry file exists.

**Homebrew zap:** `brew uninstall --cask --zap toolport` quits Toolport and runs
this same removal (`--remove-data --confirm`) while the app is still installed,
saving the report to `~/Toolport-removal-report.json`. It then trashes the app
storage listed above and removes `~/Library/Application Support/Toolport` and
`Conduit` only if the removal left them empty. If the removal reports leftovers,
the zap continues, prints the report path and keeps whatever the report lists,
including the registry needed for a retry. Plain `brew uninstall` never removes
data.

Native client files, servers and installed package binaries are kept. The package manager removes installed binaries in step 2. The action
uses the current data directory, including a custom `TOOLPORT_DATA_DIR`, an
existing `Conduit` directory or the separate `Toolport-dev` directory.

## Keeping Toolport

To move a client off Toolport without uninstalling, use **Disconnect** on that
client only. To keep a client connected while finding tools on demand instead
of sending the full tool list, choose lazy discovery for that client on the
**Clients** page. `TOOLPORT_DISCOVERY=full` sends every definition and is only a
standalone process default; shared daemon sessions use the saved client setting.
See [Configuration](configuration.md).
