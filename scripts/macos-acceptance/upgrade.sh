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
SECRET_SHA="sha256:$(shasum -a 256 <"$SECRET_FILE" | cut -d' ' -f1)"
SERVER_ID="$(cat "$EVIDENCE/server-id" 2>/dev/null || echo none)"
before="$(/usr/libexec/PlistBuddy -c 'Print :CFBundleShortVersionString' "$APP_PATH/Contents/Info.plist")"

python3 "$HERE/clients.py" snapshot "$HOME" "$MANIFEST" "$SNAP" >/dev/null
cp "$DATA_DIR/registry.json" "$EVIDENCE/logs/registry-before-upgrade.json" 2>/dev/null || true
if quit_app; then pass "previous version quits"; else fail "previous version quits" "had to be killed"; fi

# install-dmg.sh records its own rows under "install"; it replaces the app.
bash "$HERE/install-dmg.sh" "$TAG" Toolport_aarch64-apple-darwin.dmg || STEP_FAILED=1
after="$(/usr/libexec/PlistBuddy -c 'Print :CFBundleShortVersionString' "$APP_PATH/Contents/Info.plist")"
info "versions" "$before -> $after"

bash "$HERE/first-run.sh" upgraded || STEP_FAILED=1
sleep 20 # let the new version finish any startup migration before comparing

python3 "$HERE/clients.py" unchanged "$HOME" "$SNAP" | sed "s/^clients\t/upgrade\t/" >>"$RESULTS"
grep "^upgrade" "$RESULTS" | grep -q $'\tFAIL\t' && STEP_FAILED=1
if cmp -s "$DATA_DIR/registry.json" "$EVIDENCE/logs/registry-before-upgrade.json"; then
  info "registry.json after upgrade" "unchanged"
else
  cp "$DATA_DIR/registry.json" "$EVIDENCE/logs/registry-after-upgrade.json" 2>/dev/null || true
  info "registry.json after upgrade" "rewritten by the new version (logs/registry-after-upgrade.json)"
fi

tmo 60 "$GATEWAY" --selftest-secrets >"$EVIDENCE/logs/selftest-secrets-upgraded.log" 2>&1
if grep -Eq "^OK +$SERVER_ID :: MOCK_API_TOKEN" "$EVIDENCE/logs/selftest-secrets-upgraded.log"; then
  pass "keychain token readable after the upgrade without a prompt"
else
  fail "keychain token readable after the upgrade without a prompt" "$(tr '\n' ' ' <"$EVIDENCE/logs/selftest-secrets-upgraded.log" | cut -c1-300)"
fi
no_prompt_check "after upgrade"

for entry in "$EVIDENCE"/client-entries-previous/*.json; do
  [[ -e "$entry" ]] || continue
  cid="$(basename "$entry" .json)"
  out="$EVIDENCE/logs/tool-call-upgraded-$cid.json"
  MCP_CALL_TIMEOUT_MS=90000 node "$HERE/mcp-call.mjs" "$entry" check_secret >"$out" 2>&1 &
  call=$!
  approved="$(approve_pending 95 "$call")"
  wait "$call"
  rc=$?
  if [[ $rc -eq 0 ]] && grep -qF "$SECRET_SHA" "$out"; then
    pass "tool call through the old $cid entry after the upgrade" "approvals=$approved"
  else
    fail "tool call through the old $cid entry after the upgrade" "exit $rc (logs/tool-call-upgraded-$cid.json)"
  fi
done
finish_step
