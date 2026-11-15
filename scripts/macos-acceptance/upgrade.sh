#!/usr/bin/env bash
# Upgrade a configured install of the previous release to the release under
# test the way a dmg user does (quit, replace the app in Applications, open
# it), then check that client configs were not touched and that the keychain
# token and tool calls still work.
#
# usage: upgrade.sh <tag> <client-manifest.json>
set -uo pipefail
TAG="$1"
MANIFEST="$2"
AREA="upgrade"
HERE="$(cd "$(dirname "$0")" && pwd)"
# shellcheck source=common.sh
source "$HERE/common.sh"
# shellcheck source=ui.sh
source "$HERE/ui.sh"
SNAP="$EVIDENCE/client-snapshot-before-upgrade.json"
SERVER_ID="$(cat "$EVIDENCE/server-id" 2>/dev/null)"
version_of() { /usr/libexec/PlistBuddy -c 'Print :CFBundleShortVersionString' "$APP_PATH/Contents/Info.plist" 2>/dev/null; }
before="$(version_of)" || fail "previous version readable" "no CFBundleShortVersionString in $APP_PATH"

if ! python3 "$HERE/clients.py" snapshot "$HOME" "$MANIFEST" "$SNAP" >/dev/null 2>"$EVIDENCE/logs/snapshot-before-upgrade.err"; then
  fail "client configs snapshotted before the upgrade" "$(tail -c 300 "$EVIDENCE/logs/snapshot-before-upgrade.err")"
  finish_step
fi
cp "$DATA_DIR/registry.json" "$EVIDENCE/logs/registry-before-upgrade.json" ||
  fail "registry saved before the upgrade" "no $DATA_DIR/registry.json after setting up the previous release"
quit_check "previous version quits"

# install-dmg.sh records its own rows under "install"; it replaces the app.
bash "$HERE/install-dmg.sh" "$TAG" Toolport_aarch64-apple-darwin.dmg || STEP_FAILED=1
after="$(version_of)"
if [[ -n "$after" && "v$after" == "$TAG" ]]; then
  pass "installed version is the release under test" "$before -> $after"
else
  fail "installed version is the release under test" "$before -> ${after:-unreadable}, expected ${TAG#v}"
fi

bash "$HERE/first-run.sh" upgraded || STEP_FAILED=1
sleep 20 # let the new version finish any startup migration before comparing

# Launch at login was turned on with the previous release; the login item must
# still start the upgraded app.
state="$(login_item_state)"
if [[ "$state" == ok* ]]; then
  pass "login item still starts Toolport after the upgrade" "$state"
elif [[ "$state" == absent ]]; then
  if [[ -f "$EVIDENCE/logs/login-item-previous.plist" ]]; then
    fail "login item still starts Toolport after the upgrade" "the upgrade removed $LOGIN_PLIST"
  else
    untested "login item still starts Toolport after the upgrade" "setup of the previous release could not turn Launch at login on"
  fi
else
  fail "login item still starts Toolport after the upgrade" "$state"
fi
record_rows upgrade "client configs compared after the upgrade" python3 "$HERE/clients.py" unchanged "$HOME" "$SNAP"
if [[ ! -f "$EVIDENCE/logs/registry-before-upgrade.json" ]]; then
  :
elif [[ ! -f "$DATA_DIR/registry.json" ]]; then
  fail "registry.json kept by the upgrade" "$DATA_DIR/registry.json is gone"
elif cmp -s "$DATA_DIR/registry.json" "$EVIDENCE/logs/registry-before-upgrade.json"; then
  pass "registry.json kept by the upgrade" "unchanged"
else
  cp "$DATA_DIR/registry.json" "$EVIDENCE/logs/registry-after-upgrade.json"
  if [[ -n "$SERVER_ID" ]] && python3 -c 'import json,sys; sys.exit(0 if any(s.get("id") == sys.argv[2] for s in json.load(open(sys.argv[1])).get("servers", [])) else 1)' "$DATA_DIR/registry.json" "$SERVER_ID"; then
    pass "registry.json kept by the upgrade" "rewritten by the new version, server $SERVER_ID still there (logs/registry-after-upgrade.json)"
  else
    fail "registry.json kept by the upgrade" "rewritten without server ${SERVER_ID:-?} or unreadable (logs/registry-after-upgrade.json)"
  fi
fi

if [[ -n "$SERVER_ID" ]]; then
  selftest_check "keychain token readable after the upgrade without a prompt" "$EVIDENCE/logs/selftest-secrets-upgraded.log"
else
  fail "keychain token readable after the upgrade without a prompt" "setup of the previous release saved no server"
fi
no_prompt_check "after upgrade"

calls=0
for entry in "$EVIDENCE"/client-entries-previous/*.json; do
  [[ -e "$entry" ]] || continue
  calls=$((calls + 1))
  cid="$(basename "$entry" .json)"
  out="$EVIDENCE/logs/tool-call-upgraded-$cid.json"
  if tool_call "$entry" check_secret "$out"; then
    pass "tool call through the old $cid entry after the upgrade" "$TOOL_CALL_DETAIL"
  else
    fail "tool call through the old $cid entry after the upgrade" "$TOOL_CALL_DETAIL (logs/tool-call-upgraded-$cid.json)"
  fi
done
[[ $calls -gt 0 ]] || fail "tool calls through the old entries after the upgrade" "the previous release wrote no usable entries"
finish_step
