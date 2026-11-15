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
SERVER_ID="$(cat "$EVIDENCE/server-id" 2>/dev/null)"

quit_check "app quits"
ps -axo pid,ppid,command | grep -iE 'toolport|conduit' | grep -v grep >"$EVIDENCE/logs/processes-after-quit-$LABEL.txt"
info "note: processes left after quitting" "$(wc -l <"$EVIDENCE/logs/processes-after-quit-$LABEL.txt" | tr -d ' ') (logs/processes-after-quit-$LABEL.txt)"

tmo 60 open "$APP_PATH" >"$EVIDENCE/logs/open-relaunch-$LABEL.log" 2>&1 &
if wait_for 45 app_pid; then pass "app starts again" "pid $(app_pid)"; else fail "app starts again"; finish_step; fi
sleep 8
shot "$LABEL-second-launch" >/dev/null
no_prompt_check "second launch"

CALL_TOOL=check_secret
if [[ "${SIGNED:-1}" != "1" ]]; then
  CALL_TOOL=toolport_status
  untested "keychain token readable after relaunch" "unsigned local build"
elif [[ -z "$SERVER_ID" ]]; then
  fail "keychain token readable after relaunch" "setup saved no server, so there is no token to read"
else
  selftest_check "keychain token readable after relaunch" "$EVIDENCE/logs/selftest-secrets-relaunch-$LABEL.log"
fi

entry="$EVIDENCE/client-entries-$LABEL/$CALL_CLIENT.json"
if [[ -f "$entry" ]]; then
  out="$EVIDENCE/logs/tool-call-relaunch-$LABEL.json"
  if tool_call "$entry" "$CALL_TOOL" "$out"; then
    pass "tool call after relaunch works" "$CALL_CLIENT entry, $TOOL_CALL_DETAIL"
  else
    fail "tool call after relaunch works" "$TOOL_CALL_DETAIL (logs/tool-call-relaunch-$LABEL.json)"
  fi
else
  fail "tool call after relaunch works" "setup wrote no usable $CALL_CLIENT entry"
fi
no_prompt_check "after relaunch tool call"
finish_step
