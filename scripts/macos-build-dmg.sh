#!/usr/bin/env bash
# Usage: scripts/macos-build-dmg.sh <Toolport.app> <out.dmg>
#
# Builds the drag-to-install dmg the release ships (scripts/macos-package-ci.sh)
# and the macOS payload CI job checks on every PR. Signing and notarization stay
# with the caller.
set -euo pipefail
ASSETS="$(cd "$(dirname "${BASH_SOURCE[0]}")/../packaging/macos/dmg" && pwd)"
log()  { printf '\033[1m%s\033[0m\n' "== $* ==" >&2; }
die()  { printf '\033[31mERROR: %s\033[0m\n' "$*" >&2; exit 1; }

# Build a drag-to-install dmg: Toolport.app, an Applications shortcut, and a
# background with an arrow between them. Finder layout is cosmetic: if Finder
# scripting is unavailable the dmg still has the shortcut. The check at the end
# fails the build when the app or the shortcut is missing.
build_dmg() {
  local app="$1" out="$2" assets="$ASSETS"
  local stage rw size device check
  stage="$(mktemp -d)"
  rw="$(mktemp -u).dmg"
  ditto "$app" "$stage/Toolport.app"
  ln -s /Applications "$stage/Applications"
  mkdir "$stage/.background"
  # One TIFF with both resolutions keeps the background sharp on Retina.
  tiffutil -cathidpicheck "$assets/background.png" "$assets/background@2x.png" \
    -out "$stage/.background/background.tiff" >/dev/null
  # hdiutil's own size estimate can come up short for the larger 2.0 app
  # ("No space left on device"), so size the image from the app plus headroom.
  size=$(( $(du -sm "$stage" | cut -f1) * 5 / 4 + 64 ))
  hdiutil create -volname "Toolport" -srcfolder "$stage" -size "${size}m" -ov -format UDRW "$rw"
  device="$(hdiutil attach -readwrite -noverify -noautoopen "$rw" | awk '/^\/dev\// {print $1; exit}')"
  if ! osascript <<'OSA'
tell application "Finder"
  tell disk "Toolport"
    open
    set current view of container window to icon view
    set toolbar visible of container window to false
    set statusbar visible of container window to false
    set the bounds of container window to {200, 120, 860, 548}
    set viewOptions to the icon view options of container window
    set arrangement of viewOptions to not arranged
    set icon size of viewOptions to 128
    set text size of viewOptions to 13
    set background picture of viewOptions to file ".background:background.tiff"
    set position of item "Toolport.app" of container window to {165, 190}
    set position of item "Applications" of container window to {495, 190}
    close
    open
    update without registering applications
    delay 2
    close
  end tell
end tell
OSA
  then
    log "Finder layout for the dmg window failed; the dmg keeps its Applications shortcut"
  fi
  chmod -Rf go-w /Volumes/Toolport || true
  sync
  hdiutil detach "$device" -quiet || hdiutil detach "$device" -force
  rm -f "$out"
  hdiutil convert "$rw" -format UDZO -imagekey zlib-level=9 -o "$out"
  rm -rf "$stage" "$rw"
  check="$(mktemp -d)"
  hdiutil attach -readonly -nobrowse -noautoopen -mountpoint "$check" "$out" >/dev/null
  if [ ! -d "$check/Toolport.app" ] || [ "$(readlink "$check/Applications")" != /Applications ]; then
    hdiutil detach "$check" -quiet || true
    die "dmg is missing Toolport.app or its Applications shortcut"
  fi
  hdiutil detach "$check" -quiet
}

[ $# -eq 2 ] || die "usage: $0 <Toolport.app> <out.dmg>"
[ -d "$1" ] || die "no app bundle at $1"
build_dmg "$1" "$2"
