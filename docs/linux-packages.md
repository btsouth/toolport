# Linux system packages

Toolport 2.0 ships the GTK shell as `toolport` for Ubuntu 24.04+, Debian 13+,
Fedora 40+ (with the libraries below), and current Arch. Debian and RPM packages share one release build from Ubuntu
24.04 x86_64, packaged with pinned nFPM and one
[`nfpm.yaml`](../packaging/linux/native/nfpm.yaml). Runtime dependencies are
listed for that build's GTK 4.14, libadwaita 1.5, GLib 2.80 and glibc 2.39 floor.
Arch offers the native PKGBUILD and AUR `toolport-bin`, which repackages the GTK
deb with Arch dependencies. The Tauri AppImage is still built on Ubuntu 22.04
for older distributions and keeps its in-app updater.

## Install

```sh
curl -fsSL https://toolport.app/install.sh | bash
```

On Ubuntu 24.04+ and Debian 13+ this installs the GTK `.deb` with apt. Ubuntu
22.04 and Debian 12 automatically get the Tauri AppImage with a short explanation.
Ubuntu derivatives use their reported Ubuntu base version; other Debian
derivatives use apt's available library versions. If compatibility cannot be
established, the installer chooses AppImage. Current Arch and its derivatives
keep the signed pacman repository route.

The installer uses AppImage on RPM distributions. To install GTK manually, the
RPM requires GTK 4.14+, libadwaita 1.5+, GLib 2.80+ and glibc 2.39+, as available
on Fedora 40+. Older Fedora and RHEL 9 use AppImage; other RPM systems must meet
all four library floors before using the RPM. There is no automatic RPM install.

To inspect the installer's selection without network access or changes, download
it from [the pinned installer URL](https://toolport.app/install.sh), then run
`bash install.sh --print-plan`.

For a manual portable install, download `Toolport_<version>_amd64.AppImage` from
[Releases](https://github.com/btsouth/toolport/releases), run
`chmod +x Toolport*.AppImage`, then run the file. The installer places this build
at `~/.local/bin/toolport` (or `$XDG_BIN_HOME/toolport`) and adds a desktop entry.
If the AppImage reports missing FUSE support, install `libfuse2` on Ubuntu 22.04
or Debian 12. This is the Tauri shell with its in-app updater. GTK packages use system package updates or
manual downloads and do not check for updates in the app.

The `.deb` keeps the 1.x Tauri package name `toolport`. Its higher 2.0 version
causes an ordinary apt upgrade to replace 1.x without `Conflicts` or `Replaces`
against itself. dpkg removes the old `Toolport.desktop` and `conduit.png` files.
`/usr/bin/toolport` and `/usr/bin/conduit` become symlinks to `toolport-gtk` to
preserve CLI and launch-at-login paths. Both shells use `~/.config/Toolport`.
Native package removal restores client configs through a bounded per-user
cleanup helper; upgrades preserve connections. Debian's `remove in-favour`
replacement also preserves connections. pacman supplies no replacement flag to
`pre_remove`, so replacing `toolport` with a conflicting package such as
`toolport-bin` disconnects clients too. Reconnect them in the replacement app.
For a `.deb` update, download the new file from the release page and run
`sudo apt install ./<file>.deb`; for an `.rpm`, run
`sudo dnf install ./<file>.rpm`. There is no Toolport apt or dnf repository.
Arch repository users update with `sudo pacman -Syu`; `toolport-bin` users update
with their AUR helper or rebuild the AUR package.
React uses Tauri's bundle type for package guidance. GTK queries dpkg, rpm and
pacman for ownership of its running executable, so installing a manager on a
different distro does not select its advice. GTK does not check for new releases;
Settings links to the release page. Detection runs off the GTK main thread and
is cached for the process lifetime; generic instructions appear until it returns.
Development builds get generic instructions.

## Build and test

On an Ubuntu 24.04 build machine with Rust stable, GTK4/libadwaita headers,
pkg-config, D-Bus/OpenSSL headers, zip, curl, Node.js, rpm and Docker:

```sh
scripts/install-nfpm.sh .verify/nfpm
NFPM_BIN=.verify/nfpm/nfpm scripts/build-linux-packages.sh
gh release download v1.24.0 -R btsouth/toolport -p '*.deb' -D .verify/native-packages/old
scripts/test-linux-packages.sh .verify/native-packages/*.deb .verify/native-packages/*.rpm .verify/native-packages/old/*.deb
```

The reusable `linux-packages.yml` job runs these steps for relevant Rust, Cargo,
packaging, package script and workflow changes on PRs and main/next/2.0 pushes.
The merge gate accepts an intentional skip for unrelated changes. Release tags
always run the job and attach only tested packages to a draft.
Container tests install the published v1.24.0 `.deb` on Ubuntu 24.04, seed a
registry/server, client configuration and opaque credential fixtures, then apt
upgrade and compare file hashes, modes and owners. Debian 13 and Fedora get
fresh installs. All three compare the installed GTK binary to the build, check
desktop metadata and library resolution, and run `toolport-gateway --version`.
Containers and image aliases are task-owned and cleaned after testing.
If the build host blocks Docker bridge DNS, set
`TOOLPORT_PACKAGE_TEST_NETWORK=host` for the test command. Tests expose no ports.
An optional fourth argument selects `ubuntu`, `debian` or `fedora` when rerunning
one install test. CI always runs all three. Fedora installs `diffutils` solely
for the binary comparison check.

These are package installation and headless smoke checks, not GTK display or
live Secret Service acceptance. Credential fixtures prove byte preservation;
they do not exercise unlocking a real keyring. The shipped preview rollback
script mirrors the Arch package and supports pacman only.
