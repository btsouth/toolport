#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
stage_root="${1:-$repo_root/target/toolport-native-root}"

case "$stage_root" in
  ""|/|"$repo_root")
    echo "error: refusing unsafe staging directory: $stage_root" >&2
    exit 1
    ;;
esac

cargo build \
  --manifest-path "$repo_root/src-tauri/Cargo.toml" \
  --release \
  --locked \
  --no-default-features \
  --features gtk-desktop \
  --bin toolport-gtk \
  --bin toolport-gateway

install -d "$stage_root/usr/bin"
install -Dm755 "$repo_root/src-tauri/target/release/toolport-gtk" \
  "$stage_root/usr/bin/toolport-gtk"
install -Dm755 "$repo_root/src-tauri/target/release/toolport-gateway" \
  "$stage_root/usr/bin/toolport-gateway"
install -Dm644 \
  "$repo_root/packaging/linux/native/com.tsout.Toolport.NativePreview.desktop" \
  "$stage_root/usr/share/applications/com.tsout.Toolport.NativePreview.desktop"
install -Dm644 \
  "$repo_root/packaging/linux/native/com.tsout.Toolport.NativePreview.metainfo.xml" \
  "$stage_root/usr/share/metainfo/com.tsout.Toolport.NativePreview.metainfo.xml"
install -Dm644 "$repo_root/src-tauri/icons/32x32.png" \
  "$stage_root/usr/share/icons/hicolor/32x32/apps/toolport.png"
install -Dm644 "$repo_root/src-tauri/icons/128x128.png" \
  "$stage_root/usr/share/icons/hicolor/128x128/apps/toolport.png"
install -Dm644 "$repo_root/src-tauri/icons/128x128@2x.png" \
  "$stage_root/usr/share/icons/hicolor/256x256/apps/toolport.png"
install -d "$stage_root/usr/share/toolport/agent-plugin"
(
  cd "$repo_root/packaging/agent-plugin"
  zip -qr "$stage_root/usr/share/toolport/agent-plugin/toolport-agent-plugin.zip" toolport
)
install -Dm644 "$repo_root/LICENSE" \
  "$stage_root/usr/share/licenses/toolport-native-preview/LICENSE"

desktop-file-validate \
  "$stage_root/usr/share/applications/com.tsout.Toolport.NativePreview.desktop"
appstreamcli validate --no-net \
  "$stage_root/usr/share/metainfo/com.tsout.Toolport.NativePreview.metainfo.xml"

echo "staged the Linux-native preview at $stage_root"
