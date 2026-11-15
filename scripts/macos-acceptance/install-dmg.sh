#!/usr/bin/env bash
# Install a published Toolport dmg the way the README describes: download it
# from GitHub Releases in a browser (simulated: the file gets the same
# com.apple.quarantine attribute Safari sets), open it, and drag Toolport.app to
# Applications (ditto keeps extended attributes the way a Finder copy does).
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
expected="$(gh api "repos/$REPO/releases/tags/$TAG" --jq ".assets[] | select(.name == \"$ASSET\") | .digest" 2>/dev/null || true)"
actual="sha256:$(shasum -a 256 "$DMG" | cut -d' ' -f1)"
if [[ -z "$expected" || "$expected" == "null" ]]; then
  info "$ASSET digest" "release API has no digest; sha256 is $actual"
elif [[ "$expected" == "$actual" ]]; then
  pass "$ASSET digest matches the release" "$actual"
else
  fail "$ASSET digest matches the release" "expected $expected, downloaded $actual"
fi

# What Safari writes: flags 0083 (downloaded, user-approved later), a hex time,
# the agent name and an event UUID.
xattr -w com.apple.quarantine "0083;$(printf '%x' "$(date +%s)");Safari;$(uuidgen)" "$DMG"
info "$ASSET quarantine set" "$(xattr -p com.apple.quarantine "$DMG")"

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
# A plain hdiutil volume has no Applications shortcut; the README tells users
# to drag the app to Applications, so record whether the dmg offers one.
if [[ -L "$MNT/Applications" ]]; then
  info "$ASSET Applications shortcut" "present"
else
  info "$ASSET Applications shortcut" "absent: the dmg window shows only Toolport.app, with no drag target"
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
      tmo 180 osascript -e "tell application \"Finder\" to duplicate (POSIX file \"$MNT/Toolport.app\" as alias) to (POSIX file \"/Applications\" as alias) with replacing" >/dev/null 2>&1 ||
        ditto "$MNT/Toolport.app" /Applications/Toolport.app
      info "fallback copy" "copied without a drag; the app may run translocated"
    fi
  fi
  q="$(xattr -p com.apple.quarantine /Applications/Toolport.app 2>/dev/null || true)"
  if [[ -n "$q" ]]; then
    info "installed app is quarantined" "$q (as after a Finder drag from a downloaded dmg)"
  else
    info "installed app is quarantined" "no quarantine attribute was propagated"
  fi
else
  # Keep a copy for inspection without installing it.
  rm -rf "${RUNNER_TEMP:-/tmp}/inspect/$ASSET"
  mkdir -p "${RUNNER_TEMP:-/tmp}/inspect/$ASSET"
  ditto "$MNT/Toolport.app" "${RUNNER_TEMP:-/tmp}/inspect/$ASSET/Toolport.app"
fi
hdiutil detach "$MNT" >/dev/null 2>&1 || hdiutil detach -force "$MNT" >/dev/null 2>&1 || true
finish_step
