# Uninstall

Toolport installs a gateway entry into each AI client's own config file and keeps
its state in a per-user data directory. A clean removal has two parts: disconnect
the clients while Toolport is still installed, then remove the app and its data.

## 1. Disconnect your clients

Toolport can only take back what it wrote while it is still installed, so do this
first.

1. Open Toolport and go to **Clients**.
2. For every client that shows **connected to Toolport**, open it and click
   **Disconnect**. That removes the Toolport entry from that client's config file
   and leaves your other servers and the rest of the file untouched.
3. Turn off **Launch at login** in Settings.

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
