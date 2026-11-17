#!/usr/bin/env bash
# Install a published Toolport dmg the way the README describes: download it
# from GitHub Releases in a browser (simulated: the file gets the same
# com.apple.quarantine attribute Safari sets), open it, and drag Toolport.app to
# Applications with the mouse in Finder.
#
# usage: install-dmg.sh <tag> <asset-name> [--no-install]
# env:   GH_TOKEN (read-only), GITHUB_REPOSITORY, EVIDENCE
set -uo pipefail
AREA="install"
# shellcheck source=common.sh
source "$(dirname "$0")/common.sh"

TAG="$1"
ASSET="$2"
INSTALL=1
[[ "${3:-}" == "--no-install" ]] && INSTALL=0
REPO="${GITHUB_REPOSITORY:-btsouth/toolport}"
DL="$HOME/Downloads"
mkdir -p "$DL"
DMG="$DL/$ASSET"
URL="https://github.com/$REPO/releases/download/$TAG/$ASSET"

log "Downloading $URL"
if ! curl -fsSL --retry 3 -o "$DMG" "$URL"; then
  fail "$ASSET download" "could not download $URL"
  finish_step
fi
pass "$ASSET download" "$(du -h "$DMG" | cut -f1)"

# GitHub records a sha256 digest for every release asset; compare against it.
# An API error is a failure, not a missing digest.
if ! expected="$(gh api "repos/$REPO/releases/tags/$TAG" --jq ".assets[] | select(.name == \"$ASSET\") | .digest" 2>"$EVIDENCE/logs/digest-api-$ASSET.err")"; then
  fail "$ASSET digest matches the release" "could not read the release from the API: $(head -c 200 "$EVIDENCE/logs/digest-api-$ASSET.err")"
  expected="error"
fi
actual="sha256:$(shasum -a 256 "$DMG" | cut -d' ' -f1)"
if [[ "$expected" == "error" ]]; then
  :
elif [[ -z "$expected" || "$expected" == "null" ]]; then
  untested "$ASSET digest matches the release" "the release API lists no digest for this asset; downloaded sha256 is $actual"
elif [[ "$expected" == "$actual" ]]; then
  pass "$ASSET digest matches the release" "$actual"
else
  fail "$ASSET digest matches the release" "expected $expected, downloaded $actual"
fi

# What Safari writes: flags 0083 (downloaded, user-approved later), a hex time,
# the agent name and an event UUID. The rest of the job depends on it.
qval="0083;$(printf '%x' "$(date +%s)");Safari;$(uuidgen)"
if xattr -w com.apple.quarantine "$qval" "$DMG" && [[ "$(xattr -p com.apple.quarantine "$DMG")" == "$qval" ]]; then
  pass "$ASSET marked as a browser download" "$qval"
else
  fail "$ASSET marked as a browser download" "could not set com.apple.quarantine"
fi

check "$ASSET codesign" codesign --verify --strict --verbose=2 "$DMG"
check "$ASSET Gatekeeper (open)" spctl -a -vv -t open --context context:primary-signature "$DMG"
check "$ASSET stapled ticket" xcrun stapler validate "$DMG"
codesign -dvv "$DMG" >"$EVIDENCE/logs/$ASSET.codesign-d.log" 2>&1 || true

TPUI_BIN="${TPUI:-$EVIDENCE/bin/tpui}"
finder_drag_install() {
  local dlog="$EVIDENCE/logs/finder-drag-$ASSET.log" fpid src dst i want have
  {
    tmo 60 osascript -e 'on run argv' -e 'tell application "Finder"' -e 'activate' -e 'close every window' \
      -e 'set w to make new Finder window to (POSIX file (item 1 of argv) as alias)' \
      -e 'set current view of w to icon view' -e 'set bounds of w to {40, 80, 480, 500}' \
      -e 'set r to make new Finder window to (POSIX file "/" as alias)' \
      -e 'set current view of r to icon view' -e 'set bounds of r to {520, 80, 1000, 500}' -e 'end tell' -e 'end run' "$MNT"
    sleep 2
    fpid="$(pgrep -x Finder | head -1)"
    "$TPUI_BIN" find "$fpid" "Toolport" | tee "$EVIDENCE/logs/finder-items-$ASSET.txt"
    "$TPUI_BIN" find "$fpid" "Applications" | tee -a "$EVIDENCE/logs/finder-items-$ASSET.txt"
  } >"$dlog" 2>&1
  shot "finder-drag-before-$ASSET" >/dev/null || true
  # Drag the app's icon onto the Applications folder icon in the root window.
  pair="$(python3 "$(dirname "$0")/finder_targets.py" "$EVIDENCE/logs/finder-items-$ASSET.txt")"
  src="${pair% *}"
  dst="${pair#* }"
  if [[ -z "${src:-}" || -z "${dst:-}" ]]; then
    echo "could not locate the icon or the Applications sidebar row" >>"$dlog"
    return 1
  fi
  echo "drag $src -> $dst" >>"$dlog"
  "$TPUI_BIN" drag "${src%,*}" "${src#*,}" "${dst%,*}" "${dst#*,}" >>"$dlog" 2>&1
  want="$(du -sk "$MNT/Toolport.app" | cut -f1)"
  for i in $(seq 1 120); do
    # Upgrades: Finder asks to replace the existing app.
    "$TPUI_BIN" press "$fpid" "Replace" >>"$dlog" 2>&1 && shot "finder-drag-replace-$ASSET" >/dev/null
    have="$(du -sk /Applications/Toolport.app 2>/dev/null | cut -f1)"
    if [[ -n "$have" && "$have" -ge "$want" ]] && cmp -s "$MNT/Toolport.app/Contents/MacOS/conduit" /Applications/Toolport.app/Contents/MacOS/conduit; then
      sleep 3
      break
    fi
    sleep 1
  done
  shot "finder-drag-after-$ASSET" >/dev/null || true
  tmo 20 osascript -e 'tell application "Finder" to close every window' >/dev/null 2>&1 || true
  [[ -d /Applications/Toolport.app ]] && cmp -s "$MNT/Toolport.app/Contents/MacOS/conduit" /Applications/Toolport.app/Contents/MacOS/conduit
}

MNT="$(mktemp -d /tmp/toolport-dmg.XXXXXX)"
if ! hdiutil attach -nobrowse -readonly -mountpoint "$MNT" "$DMG" >"$EVIDENCE/logs/$ASSET.attach.log" 2>&1; then
  fail "$ASSET mounts" "hdiutil attach failed (logs/$ASSET.attach.log)"
  finish_step
fi
pass "$ASSET mounts" "$MNT"
ls -la "$MNT" >"$EVIDENCE/logs/$ASSET.volume.txt" 2>&1
if [[ -d "$MNT/Toolport.app" ]]; then
  pass "$ASSET contains Toolport.app"
else
  fail "$ASSET contains Toolport.app" "volume has: $(ls "$MNT" | tr '\n' ' ')"
fi
# The README tells users to drag the app to Applications: the dmg must offer
# the drag target (scripts/macos-build-dmg.sh). Releases before 2.3.6 had no
# such layout, so the previous release is only noted, never failed, on it.
layout_check() {
  if [[ -n "${PREVIOUS_TAG:-}" && "$TAG" == "$PREVIOUS_TAG" ]]; then
    untested "$1" "the previous release ($TAG) is not under test: $2"
  else
    fail "$1" "$2"
  fi
}
if [[ "$(readlink "$MNT/Applications")" == /Applications ]]; then
  pass "$ASSET has an Applications shortcut"
else
  layout_check "$ASSET has an Applications shortcut" "absent: the dmg window shows only Toolport.app, with no drag target"
fi
if [[ -f "$MNT/.background/background.tiff" ]]; then
  pass "$ASSET has its drag-to-install background"
else
  layout_check "$ASSET has its drag-to-install background" "no .background/background.tiff"
fi

if [[ $INSTALL -eq 1 ]]; then
  # Drag Toolport.app onto Applications in the Finder sidebar with the mouse,
  # as a user does. Finder records the move, which keeps Gatekeeper from
  # running the app from a random App Translocation folder.
  if finder_drag_install; then
    pass "dragged to Applications in Finder"
  else
    fail "dragged to Applications in Finder" "see logs/finder-drag-$ASSET.log and screenshots/finder-drag-*"
    if [[ ! -d /Applications/Toolport.app ]]; then
      # Keep the rest of the job running; the drag failure above already fails it.
      if ditto "$MNT/Toolport.app" /Applications/Toolport.app; then
        info "note: fallback copy" "copied with ditto instead of a drag; the app may run translocated"
      else
        fail "app copied to Applications" "both the Finder drag and ditto failed"
      fi
    fi
  fi
  q="$(xattr -p com.apple.quarantine /Applications/Toolport.app 2>/dev/null || true)"
  if [[ -n "$q" ]]; then
    info "note: installed app is quarantined" "$q (as after a Finder drag from a downloaded dmg)"
  else
    info "note: installed app is quarantined" "no quarantine attribute was propagated"
  fi
else
  # Keep a copy for inspection without installing it.
  rm -rf "${RUNNER_TEMP:-/tmp}/inspect/$ASSET"
  mkdir -p "${RUNNER_TEMP:-/tmp}/inspect/$ASSET"
  if ! ditto "$MNT/Toolport.app" "${RUNNER_TEMP:-/tmp}/inspect/$ASSET/Toolport.app"; then
    fail "$ASSET app copied for inspection" "ditto failed"
  fi
fi
hdiutil detach "$MNT" >/dev/null 2>&1 || hdiutil detach -force "$MNT" >/dev/null 2>&1 || true
finish_step
