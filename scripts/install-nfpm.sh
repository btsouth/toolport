#!/usr/bin/env bash
# Pinned standalone packager shared by CI and local package builds.
set -euo pipefail
outdir=${1:?usage: install-nfpm.sh OUTDIR}
mkdir -p "$outdir"
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
curl -fsSL --retry 2 --max-time 120 \
  https://github.com/goreleaser/nfpm/releases/download/v2.47.0/nfpm_2.47.0_Linux_x86_64.tar.gz \
  -o "$tmp/nfpm.tar.gz"
printf '%s  %s\n' \
  0660ca602b2d2d2ae4781a06c692b3eeb9d437ffea05b831d76e41f4a3188783 \
  "$tmp/nfpm.tar.gz" | sha256sum -c -
tar -xzf "$tmp/nfpm.tar.gz" -C "$outdir" nfpm
"$outdir/nfpm" --version
