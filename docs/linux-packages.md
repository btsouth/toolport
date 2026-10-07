# Linux system packages

Toolport 2.0 ships the GTK shell as `toolport` for Ubuntu 24.04+, Debian 13,
Fedora, and Arch. Debian and RPM packages share one release build from Ubuntu
24.04 x86_64, packaged with pinned nFPM and one
[`nfpm.yaml`](../packaging/linux/native/nfpm.yaml). Runtime dependencies are
listed for that build's GTK 4.14, libadwaita 1.5, GLib 2.80 and glibc 2.39 floor.
Arch uses the native PKGBUILD. The Tauri AppImage is still built on Ubuntu 22.04
for older distributions and keeps its in-app updater.

The `.deb` keeps the 1.x Tauri package name `toolport`. Its higher 2.0 version
causes an ordinary apt upgrade to replace 1.x without `Conflicts` or `Replaces`
against itself. dpkg removes the old `Toolport.desktop` and `conduit.png` files.
`/usr/bin/toolport` and `/usr/bin/conduit` become symlinks to `toolport-gtk` to
preserve CLI and launch-at-login paths. Both shells use `~/.config/Toolport`;
there are no maintainer scripts that rewrite user data or access a keyring.
Update system installs through apt, dnf or pacman.

## Build and test

On an Ubuntu 24.04 build machine with Rust stable, GTK4/libadwaita headers,
pkg-config, D-Bus/OpenSSL headers, zip, curl and Docker:

```sh
scripts/install-nfpm.sh .verify/nfpm
NFPM_BIN=.verify/nfpm/nfpm scripts/build-linux-packages.sh
gh release download v1.24.0 -R btsouth/toolport -p '*.deb' -D .verify/native-packages/old
scripts/test-linux-packages.sh .verify/native-packages/*.deb .verify/native-packages/*.rpm .verify/native-packages/old/*.deb
```

The reusable `linux-packages.yml` job runs these steps on PRs, main and next/2.0
pushes, and release tags. Release jobs attach only tested packages to a draft.
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
