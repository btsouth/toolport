# Upgrade from Toolport 1.24.x to 2.0

Install the 2.0 package for your platform, then restart Toolport and your AI
clients. Keep a copy of your Toolport data folder and client configs before
upgrading, especially if you may need to return to 1.24.

## What the first start changes

Toolport upgrades `registry.json` from schema v1 through v2 to v3. Before the
migration, it saves the original bytes beside the registry as
`registry.json.v1-<time>.bak`. A failed migration leaves the original registry
untouched. Ordinary saves do not prune this snapshot; later schema migrations
keep the newest five migration snapshots in total.

Release builds normally use these data folders:

- Linux: `~/.config/Toolport` (or `$XDG_CONFIG_HOME/Toolport`).
- macOS: `~/Library/Application Support/Toolport`.
- Windows: `%USERPROFILE%\AppData\Roaming\Toolport`.

`TOOLPORT_DATA_DIR` overrides the data folder; `TOOLPORT_REGISTRY` overrides
the registry file path.
An older `Conduit` data folder is renamed when safe; otherwise Toolport continues
using it. Use the folder that contains your actual registry.

## What carries over

- **Servers and credentials.** Server definitions and secret references remain.
  Registry migration does not rewrite OS keychain credentials.
- **Client connections.** Registry migration preserves client assignments and
  does not edit client files. Existing Shared HTTP and mcp-remote entries retain
  their transport and authentication at startup. Older Conduit names and paths
  may still be repaired separately during desktop startup.
- **Access sets.** Your existing profiles, tool scopes and folder assignments
  remain. The migration preserves each client's existing access, including
  clients without an explicit set. Servers enabled in any old profile become
  globally enabled. Turning a server off globally now removes it from every
  access set. The upgrade notice offers to stop old gateways; restart affected
  clients so they use the new access controls.
- **Saved approvals.** Existing Always allow entries remain. Approval remains
  bound to the tool definition, so a changed definition can need approval again.
- **Safety.** If neither approval nor confirmation was enabled, Safety becomes
  Off. Human approval or destructive-call confirmation becomes Ask. Existing destructive
  tool blocks, drift quarantine, injection blocking and holds on untrusted
  servers are kept separately under **Kept from 1.x**. Choosing a safety level
  replaces those extra protections. New installs start at Ask. Team policy can
  require a higher level.
- **Code Mode and environment.** Your Code Mode choice remains; if absent in a
  v1 registry it stays on, as in 1.x. New installs start with it off. Existing
  local servers keep full shell environment inheritance unless already set
  explicitly. New local servers use an environment allowlist by default.
- **Discovery.** Explicit per-client and global choices remain, including the
  old `lazyDiscovery: false` choice of Full. If you were on the 1.x default,
  every client keeps Lazy, and Clients shows it as "Auto (lazy)". New installs
  use Auto, which picks Full or Lazy per client. You can pick a mode for any
  client in Clients.

## What is available in 2.0

- Client stdio connections use small adapters backed by one shared host daemon
  in normal operation. Private and in-process fallbacks still exist. The old
  per-client gateway topology setting is retired.
- Lazy discovery exposes tool search and call tools; Grouped adds a browse tool
  per server, and Full exposes the scoped catalog. See
  [discovery modes](grouped-discovery.md). Search returns 10 candidates by
  default, with a maximum of 50.
- Safety is selected as Off, Ask or Strict. Ask holds destructive calls;
  Strict also blocks destructive tools, quarantines definition drift and blocks
  detected injection. Kept 1.x protections and team policy can add restrictions.
- Personal sync carries server definitions and explicitly portable nonsecret
  values between machines. Secrets and execution approvals stay local. Synced
  servers that require execution, secret or private-network consent stay disabled
  until reviewed locally. See [personal sync](personal-sync.md).
- Activity shows gateway calls and approval history. Linux native packages use
  the GTK shell; the AppImage, macOS and Windows use the Tauri shell.

## Removed features

Collections are gone. Add individual servers from Catalog or import a setup;
servers already added remain in your registry.

Agent rules, agent permissions and the guard hook, agent activity hooks,
routines and agent control are also removed. If you used them, an upgrade
notice names the removed features. Manage rules and permissions in your client
instead; Activity records gateway calls rather than agent hook activity.

Before removing their registry fields, Toolport exports authored settings to
`exports/` in the data folder without overwriting an existing export:

- `rules-<date>.md` for agent rules and recorded rule-file information.
- `agent-permissions-<date>.json` for agent permission rules.
- `routines-<date>.json` for routines; the original `routines.json` remains.

Client rule files and hook files are left in place by this schema migration.
Review them in the client if you want the old rules or hooks removed.

## Package notes

- **Arch and Omarchy:** update the signed repository package with
  `sudo pacman -Syu`. AUR `toolport-bin` uses your AUR helper. Changing between
  conflicting package names can disconnect clients during removal; reconnect
  them in the replacement app.
- **Debian and Ubuntu:** the GTK `.deb` uses the same `toolport` package name as
  1.x. Install it with `sudo apt install ./<file>.deb`. The installer selects
  GTK for Ubuntu 24.04+ and Debian 13+, and AppImage for older systems or when compatibility cannot
  be established. `/usr/bin/toolport` and `/usr/bin/conduit` remain launch
  aliases in the GTK package.
- **RPM:** install a compatible GTK package with
  `sudo dnf install ./<file>.rpm`. It requires GTK 4.14+, libadwaita 1.5+,
  GLib 2.80+ and glibc 2.39+. The installer uses AppImage on RPM distributions.
- **AppImage:** replace the portable binary with the new AppImage or use its
  in-app updater. It retains the Tauri shell. Native GTK packages use package
  updates or manual downloads and have no in-app updater. There is no Toolport
  apt or dnf repository. See [Linux packages](linux-packages.md).
- **macOS:** use the DMG for Apple Silicon or Intel, or the Homebrew cask.
  Replace the app in Applications; keep its Application Support data.
- **Windows:** run the NSIS `-setup.exe` installer. Keep the existing Toolport
  data folder. Toolport itself does not ship as MSIX.

## Return to 1.24

1. Quit the AI clients, Toolport and its gateways, including the shared daemon,
   so no 2.0 process can rewrite the restored file.
2. Copy the migrated `registry.json` somewhere safe. Copy the newest
   `registry.json.v1-<time>.bak` over `registry.json` in the same data folder.
   This restores the pre-upgrade setup and excludes later changes made in 2.0.
   A v2 backup is not a substitute for the v1 backup.
3. Install the 1.24 package for your platform from
   [GitHub Releases](https://github.com/btsouth/toolport/releases/tag/v1.24.0).
   If you changed client connections in 2.0, review their config backups and
   restore or reconnect them with 1.24 as needed. Then restart Toolport and clients.

Released 1.x builds do not reject newer registry schemas. Do not rely on opening
1.24 against the migrated registry: fields removed by 2.0 are no longer there.
If the v1 snapshot is missing, use your own pre-upgrade backup.

On Arch and Omarchy, the packaged rollback helper can stop Toolport processes,
keep the migrated registry and restore the v1 snapshot. Give it a known 1.24
package explicitly:

```sh
/usr/share/toolport/toolport-preview-rollback.sh --package /path/to/toolport-1.24.0-1-x86_64.pkg.tar.zst
```

Without `--package`, it chooses a cached 1.x package or installs the repository's
current `toolport`, which may no longer be 1.24. It does not restore client configs.

## Review an older HTTP client connection

Existing Shared HTTP and mcp-remote client connections keep working unchanged at
startup. New connections use stdio and put no bearer credential on their argv.

When an older Toolport connection has a literal bearer credential in its process
arguments, Clients offers **Review migration** (GTK: **Review migration to stdio**).
The confirmation explains the replacement before anything is written. Toolport
backs up the client config first, replaces only its gateway entry with the stdio
command, and keeps other native MCP entries in place. Customized gateway commands,
arguments and headers require that same explicit confirmation. Cancel leaves the
config untouched. Restart the client after confirming.

The backup is the recovery path if you need the previous connection. Review it
privately: older backups can contain the original credential. Do not share it.
