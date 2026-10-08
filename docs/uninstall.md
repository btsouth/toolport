# Uninstall

Toolport installs a gateway entry into each AI client's own config file and keeps
its state in a per-user data directory. A clean removal has two parts: disconnect
the clients while Toolport is still installed, then remove the app and its data.

## 1. Disconnect your clients

In **Settings**, choose **Remove Toolport from all clients** while Toolport is
still installed. Review the per-client results and resolve any errors before
removing the app or its data. You can also run `toolport-gateway --disconnect-all`
as your normal user; `--dry-run` lists the affected clients without changing them.
Do not run this command with sudo. Untouched configs return to their exact original
bytes; subsequent unrelated edits are preserved or reported as conflicts.
Turn off **Launch at login** in Settings.

Windows NSIS and native GTK deb/rpm/Arch packages attempt the same operation before
removing their gateway. Failure is logged with recovery instructions and removal
continues. Keep Toolport's data on failure, reinstall it, and retry the action above.
Linux hooks enumerate accounts with `~/.config/Toolport` or `~/.config/Conduit`
and switch to each account without a login shell. For a custom `XDG_CONFIG_HOME`
or `TOOLPORT_DATA_DIR`, use the in-app action or CLI with that environment first.
Upgrades skip the removal action. Windows installers defer while client gateways
are open and name the affected clients: close their MCP sessions and retry, or
cancel to install later. They never force-kill gateways by name.

**AppImage, Tauri .deb, and macOS:** use the in-app action before deleting the app.
AppImage and dragging a macOS app to Trash have no uninstall hook. The native GTK
.deb is distinct from the Tauri .deb. The Homebrew tap is `btsouth/homebrew-toolport`;
its cask also requires the action before uninstall or zap. Homebrew runs uninstall
preflight blocks during upgrades without exposing an upgrade flag, so the cask
uses a manual action to preserve client connections during updates. On macOS the
CLI is `"/Applications/Toolport.app/Contents/MacOS/toolport-gateway" --disconnect-all`
(adjust the app path if needed).

## 2. Remove the app

- **Windows:** Settings > Apps > Installed apps > Toolport > Uninstall, or run
  `winget uninstall Toolport.Toolport`. The NSIS uninstaller removes the app and its
  bundled gateway.
- **macOS:** quit Toolport, then drag `Toolport.app` from Applications to the Trash.
  With the Homebrew cask, run `brew uninstall --cask toolport`.
- **Linux:** use the package manager that installed it:
  - pacman repository: `sudo pacman -R toolport`. See
    [Arch and Omarchy](arch-pacman-repo.md) for removing the repo and key.
  - `.deb`: `sudo apt remove toolport` (or `sudo dpkg -r toolport`).
  - Native GTK `.rpm`: `sudo dnf remove toolport`.
  - AppImage: quit Toolport and delete the `.AppImage` file.

## 3. Remove Toolport's data

The data directory holds the registry, audit and savings logs, cached tool lists,
the v2 migration's exports (including any pre-2.0 saved scripts), client-config backups,
and the gateway binaries Toolport published
for clients to spawn. Removing the directory removes all of it.

- **Linux:** `~/.config/Toolport`
- **macOS:** `~/Library/Application Support/Toolport`
- **Windows:** `%USERPROFILE%\AppData\Roaming\Toolport` (that is `%APPDATA%\Toolport`)

Installs from before the Conduit to Toolport rename keep a `Conduit` directory in
the same place (`~/.config/Conduit`, `~/Library/Application Support/Conduit`, or
`%APPDATA%\Conduit`). A `tauri dev` build uses `Toolport-dev` and is separate from
a normal install.

On Linux, an AppImage install can also leave `~/.config/autostart/Toolport.desktop`
behind; delete it if it is there.

## 4. Remove stored credentials

Server credentials and Team bearer tokens live in your OS keychain, not in the
config files. They use the service name `conduit-mcp` (kept from before the rename,
so removals must use it):

- **macOS:** open Keychain Access and delete the `conduit-mcp` items.
- **Windows:** open Credential Manager > Windows Credentials and delete the
  `conduit-mcp` entries.
- **Linux:** open your keyring (GNOME Keyring via Seahorse, or KWallet) and delete
  the `conduit-mcp` entries.

If Toolport ever ran without an OS keyring, it fell back to an encrypted
`secrets.enc` file inside the data directory, which step 3 already removes.

## Keeping Toolport

To move a client off Toolport without uninstalling, use **Disconnect** on that
client only. To keep a client connected but stop sending its tool definitions, set
`TOOLPORT_DISCOVERY=full` on that client's gateway entry; see
[Configuration](configuration.md).
