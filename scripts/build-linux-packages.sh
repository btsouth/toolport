#!/usr/bin/env bash
# Run on Ubuntu 24.04 x86_64 so both formats share the oldest supported ABI.
set -euo pipefail
cd "$(dirname "$0")/.."
export TOOLPORT_PACKAGE_VERSION
TOOLPORT_PACKAGE_VERSION=$(sed -n 's/^version = "\([^"]*\)"/\1/p' src-tauri/Cargo.toml | head -1)
if [ -n "${TOOLPORT_RELEASE_TAG:-}" ] && [ "${TOOLPORT_RELEASE_TAG#v}" != "$TOOLPORT_PACKAGE_VERSION" ]; then
  echo "error: release tag does not match Cargo package version" >&2
  exit 1
fi
export SOURCE_DATE_EPOCH=${SOURCE_DATE_EPOCH:-$(git log -1 --format=%ct)}
cargo build --manifest-path src-tauri/Cargo.toml --release --locked \
  --no-default-features --features gtk-desktop,search-static --bin toolport-gtk --bin toolport-gateway
outdir=.verify/native-packages
mkdir -p "$outdir"
# Match the plugin zip shipped by the native PKGBUILD.
rm -f "$outdir/toolport-agent-plugin.zip"
(cd packaging/agent-plugin && zip -qr ../../"$outdir"/toolport-agent-plugin.zip toolport)
nfpm_bin=${NFPM_BIN:-nfpm}
"$nfpm_bin" package --config packaging/linux/native/nfpm.yaml --packager deb \
  --target "$outdir/Toolport_${TOOLPORT_PACKAGE_VERSION}_amd64.deb"
"$nfpm_bin" package --config packaging/linux/native/nfpm.yaml --packager rpm \
  --target "$outdir/Toolport_${TOOLPORT_PACKAGE_VERSION}_x86_64.rpm"

node .github/scripts/package-contents.mjs "$outdir"/*.deb "$outdir"/*.rpm
