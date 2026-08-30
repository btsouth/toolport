#!/bin/bash
set -euo pipefail

version=${1:-}
version=${version#v}
if [[ ! $version =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
  echo "Usage: $0 vX.Y.Z" >&2
  exit 2
fi

repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
recipes=(
  "$repo_root/packaging/linux/native/PKGBUILD"
  "$repo_root/packaging/omarchy-pkgs/toolport/PKGBUILD"
)

for recipe in "${recipes[@]}"; do
  pkgver=$(sed -n 's/^pkgver=//p' "$recipe")
  if [[ $pkgver != "$version" ]]; then
    echo "$recipe has pkgver=$pkgver, expected $version" >&2
    exit 1
  fi
  if [[ $(grep -Ec "^sha256sums=\('[^']+'\)$" "$recipe") -ne 1 ]]; then
    echo "$recipe does not have exactly one replaceable source checksum" >&2
    exit 1
  fi
done

archive=$(mktemp)
trap 'rm -f "$archive"' EXIT
url="https://github.com/tsouth89/toolport/archive/refs/tags/v$version.tar.gz"
curl --fail --location --silent --show-error --retry 3 --output "$archive" "$url"
checksum=$(sha256sum "$archive" | cut -d' ' -f1)

for recipe in "${recipes[@]}"; do
  sed -i -E "s/^sha256sums=\('[^']+'\)$/sha256sums=('$checksum')/" "$recipe"
  grep -qx "sha256sums=('$checksum')" "$recipe"
done

cmp --silent "${recipes[0]}" "${recipes[1]}"
echo "Pinned both Omarchy recipes for v$version to $checksum"
