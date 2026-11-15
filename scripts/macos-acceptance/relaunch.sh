#!/usr/bin/env bash
# Quit Toolport and open it again: the second launch must read the keychain
# token without a prompt, and tool calls must keep working.
#
# usage: relaunch.sh <label> [client-id-for-call]
set -uo pipefail
LABEL="$1"
CALL_CLIENT="${2:-claude-code}"
AREA="second launch ($LABEL)"
HERE="$(cd "$(dirname "$0")" && pwd)"
# shellcheck source=common.sh
source "$HERE/common.sh"
# shellcheck source=ui.sh
source "$HERE/ui.sh"
SECRET_SHA="sha256:$(shasum -a 256 <"$SECRET_FILE" | cut -d' ' -f1)"
SERVER_ID="$(cat "$EVIDENCE/server-id" 2>/dev/null || echo none)"

if quit_app; then pass "app quits"; else fail "app quits" "had to be killed"; fi
ps -axo pid,ppid,command | grep -iE 'toolport|conduit' | grep -v grep >"$EVIDENCE/logs/processes-after-quit-$LABEL.txt" || true
info "processes left after quitting" "$(wc -l <"$EVIDENCE/logs/processes-after-quit-$LABEL.txt" | tr -d ' ') (logs/processes-after-quit-$LABEL.txt)"

tmo 60 open "$APP_PATH" >"$EVIDENCE/logs/open-relaunch-$LABEL.log" 2>&1 &
if wait_for 45 app_pid; then pass "app starts again" "pid $(app_pid)"; else fail "app starts again"; finish_step; fi
sleep 8
shot "$LABEL-second-launch" >/dev/null || true
no_prompt_check "second launch"

CALL_TOOL=check_secret
if [[ "${SIGNED:-1}" != "1" ]]; then
  CALL_TOOL=toolport_status
  untested "keychain token readable after relaunch" "unsigned local build"
else
tmo 60 "$GATEWAY" --selftest-secrets >"$EVIDENCE/logs/selftest-secrets-relaunch-$LABEL.log" 2>&1
if grep -Eq "^OK +$SERVER_ID :: MOCK_API_TOKEN" "$EVIDENCE/logs/selftest-secrets-relaunch-$LABEL.log"; then
  pass "keychain token readable after relaunch"
else
  fail "keychain token readable after relaunch" "$(tr '\n' ' ' <"$EVIDENCE/logs/selftest-secrets-relaunch-$LABEL.log" | cut -c1-300)"
fi
fi

entry="$EVIDENCE/client-entries-$LABEL/$CALL_CLIENT.json"
if [[ -f "$entry" ]]; then
  out="$EVIDENCE/logs/tool-call-relaunch-$LABEL.json"
  MCP_CALL_TIMEOUT_MS=90000 node "$HERE/mcp-call.mjs" "$entry" "$CALL_TOOL" >"$out" 2>&1 &
  call=$!
  approved="$(approve_pending 95 "$call")"
  wait "$call"
  rc=$?
  if [[ $rc -eq 0 ]] && { [[ "$CALL_TOOL" != check_secret ]] || grep -qF "$SECRET_SHA" "$out"; }; then
    pass "tool call after relaunch gets the keychain token" "$CALL_CLIENT entry, approvals=$approved"
  else
    fail "tool call after relaunch gets the keychain token" "exit $rc (logs/tool-call-relaunch-$LABEL.json)"
  fi
else
  untested "tool call after relaunch" "no $CALL_CLIENT entry was written"
fi
no_prompt_check "after relaunch tool call"
finish_step
