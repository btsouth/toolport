#!/bin/bash
set -euo pipefail

REPOSITORY="tsouth89/toolport"
API_URL="https://api.github.com/repos/$REPOSITORY/releases/latest"

current=$(grep -m1 '^pkgver=' PKGBUILD | cut -d= -f2- | tr -d "\"'")
tag=$(curl -fsSL -H 'Accept: application/vnd.github+json' "$API_URL" | jq -r '.tag_name // empty')
version=${tag#v}

if [[ ! $version =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
  echo "GitHub returned an unusable Toolport release tag: '$tag'" >&2
  exit 1
fi
if [[ "$(vercmp "$version" "$current")" -le 0 ]]; then
  echo '{}'
  exit 0
fi

tarball=$(mktemp)
trap 'rm -f "$tarball"' EXIT
curl -fsSL -o "$tarball" \
  "https://github.com/$REPOSITORY/archive/refs/tags/v$version.tar.gz"

jq -n \
  --arg pkgver "$version" \
  --arg sha256 "$(sha256sum "$tarball" | cut -d' ' -f1)" \
  '{pkgver: $pkgver, sha256sums: {any: [$sha256]}}'
