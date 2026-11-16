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

# Detach a mounted image. Spotlight or Finder can hold a fresh volume for a
# moment ("Resource busy"), so try a few times and force the last one.
detach() {
  local target="$1"
  for _ in 1 2 3 4 5; do
    hdiutil detach "$target" -quiet && return 0
    sleep 2
  done
  hdiutil detach "$target" -force
}

# Run `hdiutil create` or `hdiutil convert`, retrying only "Resource busy".
# CI runners sometimes still hold a fresh image file for a moment, and hdiutil
# then refuses with that error even though nothing is wrong with the content.
# Every other failure stops at once with hdiutil's own message, so a size or
# content problem is never retried away. The partial output is removed before
# each new try. Usage: hdiutil_retry <output path> <hdiutil args...>
hdiutil_retry() {
  local output="$1" try err
  shift
  for try in 1 2 3 4; do
    if err="$(hdiutil "$@" 2>&1)"; then
      [ -z "$err" ] || printf '%s\n' "$err"
      return 0
    fi
    printf '%s\n' "$err" >&2
    case "$err" in
      *"Resource busy"*) ;;
      *) die "hdiutil $1 failed: $err" ;;
    esac
    [ "$try" -lt 4 ] || break
    log "hdiutil $1: Resource busy on try $try of 4, retrying in $((try * 5))s"
    rm -rf "$output"
    sleep $((try * 5))
  done
  die "hdiutil $1 still failed after 4 tries: $err"
}

# Build a drag-to-install dmg: Toolport.app, an Applications shortcut, and a
# background with an arrow between them. Finder layout is cosmetic: if Finder
# scripting is unavailable the dmg still has the shortcut. The check at the end
# fails the build when the app or the shortcut is missing.
build_dmg() {
  local app="$1" out="$2" assets="$ASSETS"
  local stage rw size attach_out device volume check
  stage="$(mktemp -d)"
  rw="$(mktemp -u).dmg"
  ditto "$app" "$stage/Toolport.app"
  ln -s /Applications "$stage/Applications"
  mkdir "$stage/.background"
  # macOS keeps a file-event journal in .fseventsd on any volume mounted
  # read-write, and the Finder layout below writes to this one. A no_log marker
  # present at mount time tells fseventsd not to journal this volume; the folder
  # itself is removed before the image is sealed, so the dmg ships without it.
  mkdir "$stage/.fseventsd"
  touch "$stage/.fseventsd/no_log"
  # One TIFF with both resolutions keeps the background sharp on Retina.
  tiffutil -cathidpicheck "$assets/background.png" "$assets/background@2x.png" \
    -out "$stage/.background/background.tiff" >/dev/null
  # hdiutil's own size estimate can come up short for the larger 2.0 app
  # ("No space left on device"), so size the image from the app plus headroom.
  size=$(( $(du -sm "$stage" | cut -f1) * 5 / 4 + 64 ))
  hdiutil_retry "$rw" create -volname "Toolport" -srcfolder "$stage" -size "${size}m" -ov -format UDRW "$rw"
  attach_out="$(hdiutil attach -readwrite -noverify -noautoopen "$rw")"
  device="$(printf '%s\n' "$attach_out" | awk '/^\/dev\// {print $1; exit}')"
  volume="$(printf '%s\n' "$attach_out" | awk -F '\t' '/\/Volumes\// {sub(/^[ \t]+/, "", $NF); print $NF; exit}')"
  [ -n "$volume" ] && [ -d "$volume" ] || die "could not find where the dmg volume was mounted"
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
  chmod -Rf go-w "$volume" || true
  rm -rf "$volume/.fseventsd"
  sync
  detach "$device"
  rm -f "$out"
  hdiutil_retry "$out" convert "$rw" -format UDZO -imagekey zlib-level=9 -o "$out"
  rm -rf "$stage" "$rw"
  check="$(mktemp -d)"
  hdiutil attach -readonly -nobrowse -noautoopen -mountpoint "$check" "$out" >/dev/null
  if [ ! -d "$check/Toolport.app" ] || [ "$(readlink "$check/Applications")" != /Applications ]; then
    detach "$check" || true
    die "dmg is missing Toolport.app or its Applications shortcut"
  fi
  # The release asserts the dmg against the payload manifest after notarizing,
  # which is far too late to find volume metadata. Fail here instead.
  if [ -e "$check/.fseventsd" ]; then
    detach "$check" || true
    die "dmg still carries .fseventsd (macOS file-event journal)"
  fi
  detach "$check"
}

[ $# -eq 2 ] || die "usage: $0 <Toolport.app> <out.dmg>"
[ -d "$1" ] || die "no app bundle at $1"
build_dmg "$1" "$2"
