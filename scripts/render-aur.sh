#!/usr/bin/env bash
#
# Render packaging/linux/aur/{PKGBUILD,.SRCINFO} for a published release.
#
# Why a native Arch package exists at all. It was built to route around a grey
# empty window on Arch (WebKitWebProcess aborting on EGL_BAD_PARAMETER every
# launch), which was blamed at the time on the bundled Ubuntu 22.04 WebKitGTK.
# That diagnosis was wrong. The bundled WebKitGTK is current; the AppImage was
# bundling wayland 1.20, and since AppRun puts the bundle on LD_LIBRARY_PATH the
# HOST's Mesa loaded it too and libEGL_mesa failed to link
# (`undefined symbol: wl_fixes_interface`). 1.16.0 stops bundling those
# libraries and the AppImage works on Mesa and NVIDIA alike, so this package is
# no longer the only thing that runs on Arch.
#
# It is still worth shipping: it links the host's WebKitGTK, gets security
# updates with the rest of the system, and upgrades and removes through pacman.
# That is a real preference, just not a workaround any more. This repackages the
# same .deb payload with real Arch dependencies.
#
# Both files are generated from the variables below so they cannot drift from
# each other; .SRCINFO is emitted directly rather than via `makepkg
# --printsrcinfo` so this runs on any Linux, not just Arch.
#
# Usage: [AUR_PKGREL=n] scripts/render-aur.sh <version> [outdir]
#   version     release version WITHOUT the leading v, e.g. 1.15.0
#   outdir      defaults to packaging/linux/aur
#   AUR_PKGREL  Arch package revision, default 1. Bump it when re-publishing the
#               SAME version with a corrected PKGBUILD, or pacman will treat the
#               fix as already installed.
#
# Needs curl + sha256sum and network access: the checksums must come from the
# assets that were actually published, never from a previous release.
set -euo pipefail

version=${1:-}
outdir=${2:-packaging/linux/aur}

if [ -z "$version" ]; then
  echo "usage: $0 <version> [outdir]" >&2
  exit 2
fi
version=${version#v}
# Arch pkgver forbids '-', and a prerelease would sort ABOVE the stable release
# it precedes under any mangling of it. The AUR only ever carries stable.
if ! printf '%s' "$version" | grep -qE '^[0-9]+\.[0-9]+\.[0-9]+$'; then
  echo "error: '$version' is not a stable X.Y.Z version; the AUR package is stable-only." >&2
  exit 1
fi

pkgname=toolport-bin
# pacman compares pkgver-pkgrel. Re-rendering the SAME version with a fixed
# PKGBUILD (wrong depends, moved source URL) and pkgrel still 1 reads as "already
# installed" on every machine that took the broken package, so only fresh
# installs get the fix. Bumping it is an operator decision, never automatic:
# AUR_PKGREL=2 scripts/render-aur.sh 1.15.0 ./aur
pkgrel=${AUR_PKGREL:-1}
if ! printf '%s' "$pkgrel" | grep -qE '^[1-9][0-9]*$'; then
  echo "error: AUR_PKGREL must be a positive integer, got '$pkgrel'." >&2
  exit 1
fi
repo=btsouth/toolport
url="https://github.com/$repo"
pkgdesc='One MCP endpoint for every AI client: governance, approvals, and audit for your MCP servers'
deb_name="Toolport_${version}_amd64.deb"
deb_url="$url/releases/download/v${version}/${deb_name}"
license_url="https://raw.githubusercontent.com/$repo/v${version}/LICENSE"

# Direct link-time dependencies only; webkit2gtk-4.1 already pulls libsoup3,
# glib2 and the gstreamer stack. xdotool provides libxdo, which tauri links for
# the global-shortcut/window plumbing.
depends=(webkit2gtk-4.1 gtk3 libayatana-appindicator xdotool openssl dbus hicolor-icon-theme)
optdepends=(
  'gnome-keyring: store MCP server secrets in the system keyring'
  'libsecret: Secret Service backend for saved credentials'
)

tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT

# `curl --retry` does not retry a 404, and a 404 is exactly what the release
# asset CDN serves for a short window right after a release is published - which
# is the moment aur.yml runs. Retry it explicitly, then give up with an error
# that names WHICH download failed: the .deb and the LICENSE fail for completely
# different reasons, and "the release must be published first" sends the operator
# to republish a release that is already fine when it was the LICENSE path that
# moved.
fetch_sha() {
  local what=$1 target=$2 dest=$3 hint=$4
  local attempt
  for attempt in 1 2 3 4 5; do
    if curl -fsSL --retry 2 -o "$dest" "$target"; then
      sha256sum "$dest" | awk '{ print $1 }'
      return 0
    fi
    if [ "$attempt" -lt 5 ]; then
      echo "  $what not available yet (attempt $attempt); retrying in ${attempt}0s" >&2
      sleep "${attempt}0"
    fi
  done
  echo "error: could not download the $what after 5 attempts: $target" >&2
  echo "$hint" >&2
  exit 1
}

echo "fetching $deb_url" >&2
deb_sha=$(fetch_sha ".deb" "$deb_url" "$tmp/pkg.deb" \
  "The release must be PUBLISHED first; draft assets 404. Check the asset name in release.yml if the release is published and this still fails.")
echo "fetching $license_url" >&2
license_sha=$(fetch_sha "LICENSE" "$license_url" "$tmp/LICENSE" \
  "This is the LICENSE file at the tag, not a release asset: publishing the release again will not help. Check that the tag exists and that LICENSE is still at the repository root.")

mkdir -p "$outdir"

# Build the quoted array literals here rather than inside the heredoc, where a
# nested command substitution would fight the heredoc's own backslash rules.
depends_line=$(printf "'%s' " "${depends[@]}")
depends_line=${depends_line% }
optdepends_line=$(printf "'%s' " "${optdepends[@]}")
optdepends_line=${optdepends_line% }

cat >"$outdir/PKGBUILD" <<PKGBUILD
# Maintainer: South Forge AI <https://github.com/btsouth/toolport/issues>
#
# GENERATED by scripts/render-aur.sh in $url - edit the script, not this file.
#
# Repackages the official .deb rather than bundling a browser engine, so Toolport
# links the HOST WebKitGTK and upgrades with the rest of the system. The AppImage
# also works on Arch as of 1.16.0; this is the option for people who would rather
# have a real package. See the Linux notes in the project README.
pkgname=$pkgname
pkgver=$version
pkgrel=$pkgrel
pkgdesc="$pkgdesc"
arch=('x86_64')
url="$url"
license=('MIT')
depends=($depends_line)
optdepends=($optdepends_line)
provides=('toolport')
conflicts=('toolport')
# Prebuilt upstream binaries: stripping them or hunting for debug symbols only
# damages the shipped artifact.
options=('!strip' '!debug' '!emptydirs')
source=("\$pkgname-\$pkgver.deb::$deb_url"
        "LICENSE-\$pkgver::$license_url")
sha256sums=('$deb_sha'
            '$license_sha')

package() {
  # A .deb is an ar archive holding data.tar.<comp>. bsdtar (libarchive, already
  # required by makepkg) reads both layers, so this keeps working whatever
  # compression the tauri deb bundler picks - gz today.
  bsdtar -xf "\$srcdir/\$pkgname-\$pkgver.deb" -C "\$srcdir"
  bsdtar -xf "\$srcdir"/data.tar.* -C "\$pkgdir"
  install -Dm644 "\$srcdir/LICENSE-\$pkgver" "\$pkgdir/usr/share/licenses/\$pkgname/LICENSE"
}
PKGBUILD

{
  printf 'pkgbase = %s
' "$pkgname"
  printf '	pkgdesc = %s
' "$pkgdesc"
  printf '	pkgver = %s
' "$version"
  printf '	pkgrel = %s
' "$pkgrel"
  printf '	url = %s
' "$url"
  printf '	arch = x86_64
'
  printf '	license = MIT
'
  for d in "${depends[@]}"; do printf '	depends = %s
' "$d"; done
  for o in "${optdepends[@]}"; do printf '	optdepends = %s
' "$o"; done
  printf '	provides = toolport
'
  printf '	conflicts = toolport
'
  printf '	options = !strip
	options = !debug
	options = !emptydirs
'
  printf '	source = %s-%s.deb::%s
' "$pkgname" "$version" "$deb_url"
  printf '	source = LICENSE-%s::%s
' "$version" "$license_url"
  printf '	sha256sums = %s
' "$deb_sha"
  printf '	sha256sums = %s
' "$license_sha"
  printf '
'
  printf 'pkgname = %s
' "$pkgname"
} >"$outdir/.SRCINFO"

echo "wrote $outdir/PKGBUILD and $outdir/.SRCINFO for $version" >&2
echo "  deb     $deb_sha" >&2
echo "  LICENSE $license_sha" >&2
