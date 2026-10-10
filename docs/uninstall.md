# Uninstall

Toolport installs a gateway entry into each AI client's own config file and keeps
its state in a per-user data directory. A clean removal has two parts: disconnect
the clients while Toolport is still installed, then remove the app. Normal uninstall and upgrades keep your data and credentials.

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

## Optional: remove Toolport data

While Toolport is installed, choose **Remove Toolport data** in Settings. The
confirmation lists the data directory, credentials and startup entries affected,
and the report file in your home directory. Toolport closes before removal.
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

Removal first restores and disconnects clients through the same path as step 1.
A failed restoration retains recovery data. Active gateway sessions block
removal: close the listed sessions and retry. Toolport requests a graceful stop
only from authenticated daemons for this data directory.

The action removes all contents of the selected Toolport data directory,
including registry, logs, caches, migration exports, client backups, encrypted
secrets and published gateway copies in `bin/`. The empty directory may remain.
It also removes credentials under Toolport's reserved `conduit-mcp` service
on Linux Secret Service, Windows Credential Manager and macOS Keychain,
including orphaned entries, Windows chunks and the macOS encryption master key.
This service is shared by Toolport installations for the same user.
Toolport's own launch-at-login entries are removed; changed or unrecognized
entries are preserved and reported. Failed removals list the exact resource
paths, or the credential scope if the OS denied inventory access.

Native client files, servers, shared app data and installed package binaries
are kept. The package manager removes installed binaries in step 2. The action
uses the current data directory, including a custom `TOOLPORT_DATA_DIR`, an
existing `Conduit` directory or the separate `Toolport-dev` directory.

## Keeping Toolport

To move a client off Toolport without uninstalling, use **Disconnect** on that
client only. To keep a client connected but stop sending its tool definitions, set
`TOOLPORT_DISCOVERY=full` on that client's gateway entry; see
[Configuration](configuration.md).
