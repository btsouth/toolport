#!/usr/bin/env bash
# The in-app updater: a configured install of the previous release updates
# itself to the release under test through Settings > General > Check for
# updates, using the real releases/latest/download/latest.json. Then check the
# version, the relaunch, the signature of the swapped bundle, the client
# configs and the keychain token.
#
# usage: updater.sh <tag> <client-manifest.json>
# env:   TAG_IS_LATEST=true (the updater only ever offers the latest release)
set -uo pipefail
TAG="$1"
MANIFEST="$2"
WANT="${TAG#v}"
AREA="in-app update"
HERE="$(cd "$(dirname "$0")" && pwd)"
# shellcheck source=common.sh
source "$HERE/common.sh"
# shellcheck source=ui.sh
source "$HERE/ui.sh"
SNAP="$EVIDENCE/client-snapshot-before-update.json"
SERVER_ID="$(cat "$EVIDENCE/server-id" 2>/dev/null)"
version_of() { /usr/libexec/PlistBuddy -c 'Print :CFBundleShortVersionString' "$APP_PATH/Contents/Info.plist" 2>/dev/null; }

if [[ "${TAG_IS_LATEST:-}" != "true" ]]; then
  untested "in-app update to $TAG" "the updater only offers the latest release; $TAG is not it"
  finish_step
  exit 0
fi
before="$(version_of)"
[[ -n "$before" && "$before" != "$WANT" ]] || { fail "previous release installed" "installed version is '${before:-unreadable}'"; finish_step; }
if ! python3 "$HERE/clients.py" snapshot "$HOME" "$MANIFEST" "$SNAP" >/dev/null 2>"$EVIDENCE/logs/snapshot-before-update.err"; then
  fail "client configs snapshotted before the update" "$(tail -c 300 "$EVIDENCE/logs/snapshot-before-update.err")"
  finish_step
fi

[[ -n "$(app_pid)" ]] || { tmo 60 open "$APP_PATH" >/dev/null 2>&1 & wait_for 45 app_pid; sleep 6; }
old_pid="$(app_pid)"
[[ -n "$old_pid" ]] || { fail "previous release is running" "no app process"; finish_step; }
ui_activate
TPUI_TIMEOUT=20 tpui resize "$old_pid" 1000 700 >/dev/null 2>&1 # the footer version shows at 900 px and wider

log "Checking for updates"
offered=0
if ui_press "Settings" && ui_press "General" && ui_press "Check for updates"; then
  for ((i = 0; i < 90; i++)); do
    if TPUI_TIMEOUT=20 tpui find "$(app_pid)" "Update available: v$WANT" >/dev/null 2>&1; then offered=1; break; fi
    if TPUI_TIMEOUT=20 tpui find "$(app_pid)" "You're on the latest version" >/dev/null 2>&1; then offered=2; break; fi
    if TPUI_TIMEOUT=20 tpui find "$(app_pid)" "Couldn't check for updates" >/dev/null 2>&1; then offered=3; break; fi
    sleep 1
  done
fi
shot "update-offer" >/dev/null
case $offered in
1) pass "updater offers v$WANT from latest.json" "screenshots/update-offer.png" ;;
2) fail "updater offers v$WANT from latest.json" "v$before says it is on the latest version" ;;
3) fail "updater offers v$WANT from latest.json" "\"Couldn't check for updates\"" ;;
*) fail "updater offers v$WANT from latest.json" "no update dialog within 90s (or the Check for updates button was not found)" ;;
esac
[[ $offered -eq 1 ]] || finish_step

ui_press "Install and restart" -1 || { fail "Install and restart pressed" "button not found"; finish_step; }
log "Waiting for the update and relaunch"
result=""
for ((i = 0; i < 300; i++)); do
  if [[ "$(version_of)" == "$WANT" ]] && ! ps -p "$old_pid" >/dev/null 2>&1 && [[ -n "$(app_pid)" ]]; then
    result=relaunched
    break
  fi
  pid="$(app_pid)"
  if [[ -n "$pid" ]] && TPUI_TIMEOUT=20 tpui find "$pid" "Update failed" >"$EVIDENCE/logs/update-failed.txt" 2>&1; then
    result="failed: $(head -c 300 "$EVIDENCE/logs/update-failed.txt" | tr '\n' ' ')"
    shot "update-failed" >/dev/null
    break
  fi
  sleep 1
done
after="$(version_of)"
case "$result" in
relaunched)
  pass "update installs and the app relaunches" "v$before -> v$after in ${i}s, new pid $(app_pid)"
  # The updater refuses an update whose minisign signature does not verify, so
  # an installed update proves the signature check passed for the real file.
  pass "updater verified the update's signature before installing" "the signed latest.json payload was accepted"
  ;;
failed:*) fail "update installs and the app relaunches" "${result#failed: }" ;;
*) fail "update installs and the app relaunches" "after 300s: version ${after:-unreadable}, old pid $(ps -p "$old_pid" >/dev/null 2>&1 && echo running || echo gone), app pid ${pid:-none}" ;;
esac
untested "updater rejects a tampered update" "the update endpoint is compiled in; no way to serve a bad signature to a release build"
[[ "$result" == relaunched ]] || finish_step

sleep 8
ui_activate
shot "after-update" >/dev/null
if ui_wait "Toolport v$WANT" 20; then
  pass "window shows the new version" "Toolport v$WANT in the footer"
else
  fail "window shows the new version" "no \"Toolport v$WANT\" in the window"
fi
no_prompt_check "after the in-app update"

bash "$HERE/bundle-checks.sh" "$APP_PATH" arm64 "after in-app update" || STEP_FAILED=1
q="$(xattr -p com.apple.quarantine "$APP_PATH" 2>/dev/null)"
info "note: updated app quarantine" "${q:-none}"

record_rows "$AREA" "client configs compared after the update" python3 "$HERE/clients.py" updated "$HOME" "$SNAP"
if [[ -n "$SERVER_ID" ]]; then
  selftest_check "keychain token readable after the update without a prompt" "$EVIDENCE/logs/selftest-secrets-updated.log"
else
  fail "keychain token readable after the update without a prompt" "setup of the previous release saved no server"
fi
calls=0
for entry in "$EVIDENCE"/client-entries-previous/*.json; do
  [[ -e "$entry" ]] || continue
  calls=$((calls + 1))
  cid="$(basename "$entry" .json)"
  out="$EVIDENCE/logs/tool-call-updated-$cid.json"
  if tool_call "$entry" check_secret "$out"; then
    pass "tool call through the $cid entry after the update" "$TOOL_CALL_DETAIL"
  else
    fail "tool call through the $cid entry after the update" "$TOOL_CALL_DETAIL (logs/tool-call-updated-$cid.json)"
  fi
done
[[ $calls -gt 0 ]] || fail "tool calls after the update" "the previous release wrote no usable entries"
no_prompt_check "after tool calls"
finish_step
