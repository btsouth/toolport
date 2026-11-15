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
  rm -rf "/Applications/Toolport.app"
  if ditto "$MNT/Toolport.app" "/Applications/Toolport.app"; then
    pass "copied to /Applications"
  else
    fail "copied to /Applications"
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
