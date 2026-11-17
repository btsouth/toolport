#!/usr/bin/env bash
# Set Toolport up through its window the way a new user does: onboarding,
# every main screen, a local server with an API token kept in the keychain,
# and a connection to each seeded client. Then check the keychain, the client
# configs Toolport wrote, and a real tool call through each client entry.
#
# usage: ui-setup.sh <label> <client-manifest.json>
# env:   SECRET_FILE (the test token, kept outside the evidence folder)
set -uo pipefail
LABEL="$1"
MANIFEST="$2"
AREA="setup ($LABEL)"
HERE="$(cd "$(dirname "$0")" && pwd)"
# shellcheck source=common.sh
source "$HERE/common.sh"
# shellcheck source=ui.sh
source "$HERE/ui.sh"

SECRET="$(cat "$SECRET_FILE")"
[[ ${#SECRET} -ge 20 ]] || { fail "test token is ready" "$SECRET_FILE is missing or short"; finish_step; }
CLIENTS="$(client_list "$MANIFEST")" || { fail "client manifest readable" "$MANIFEST"; finish_step; }
NODE="$(command -v node)"
MOCK_DIR="$HOME/mcp-servers"
MOCK="$MOCK_DIR/mock-mcp.mjs"
mkdir -p "$MOCK_DIR"
cp "$HERE/mock-mcp.mjs" "$MOCK"
# The mock compares its token with this file in constant time (no hashing).
printf '%s' "$SECRET_FILE" >"$MOCK_DIR/expected-token-path"
REGISTRY="$DATA_DIR/registry.json"
ENTRIES="$EVIDENCE/client-entries-$LABEL"
# Signed builds call the mock server, which needs the keychain token; unsigned
# builds call Toolport's own status tool instead.
CALL_TOOL=check_secret
[[ "${SIGNED:-1}" == "1" ]] || CALL_TOOL=toolport_status
# Clients the previous release does not offer (" id " each), from onboarding.
NOT_OFFERED=""
not_offered() { [[ "$NOT_OFFERED" == *" $1 "* ]]; }

[[ -n "$(app_pid)" ]] || { fail "app is running" "start it first"; finish_step; }
ui_activate

# ── Onboarding ────────────────────────────────────────────────────────────
log "Onboarding"
if ui_wait "Welcome to Toolport" 10; then
  shot "$LABEL-onboarding-1-welcome" >/dev/null
  ui_press "Set up MCP servers" && shot "$LABEL-onboarding-2-add-servers" >/dev/null
  ui_press "Review and connect your clients" && sleep 2 && shot "$LABEL-onboarding-3-connect" >/dev/null
  detected=""
  missing=""
  while IFS=$'\t' read -r cid name; do
    if ui_wait "$name" 3; then
      detected+="$name, "
    else
      missing+="$name, "
      # The previous release only prepares the upgrade: a client it does not
      # offer yet is left out of its setup instead of failing it.
      [[ "$LABEL" == previous ]] && NOT_OFFERED+=" $cid "
    fi
  done <<<"$CLIENTS"
  if [[ -z "$missing" && -n "$detected" ]]; then
    pass "onboarding lists every seeded client" "${detected%, }"
  elif [[ "$LABEL" == previous && -n "$detected" ]]; then
    untested "onboarding lists every seeded client" "the previous release (${PREVIOUS_TAG:-?}) does not offer ${missing%, }; left out of its setup"
  else
    fail "onboarding lists every seeded client" "missing: ${missing%, } (listed: ${detected%, })"
  fi
  ui_press_any 10 "Skip for now" "Next" >/dev/null && shot "$LABEL-onboarding-4-done" >/dev/null
  if ui_press_any 60 "Start using Toolport" "Got it" "Continue without verification" >/dev/null; then
    pass "onboarding can be completed"
  else
    fail "onboarding can be completed" "no finish button found"
  fi
elif [[ "$LABEL" == previous ]]; then
  untested "onboarding on the previous release" "not shown within 10s; the earlier release may word or gate it differently"
else
  fail "onboarding shows on a clean first launch" "no \"Welcome to Toolport\" within 10s"
fi

# ── Every main screen ─────────────────────────────────────────────────────
log "Screen tour"
for screen in Servers Clients Skills Activity Settings Sync; do
  if ui_press "$screen"; then
    sleep 2
    shot "$LABEL-screen-$screen" >/dev/null
    pass "$screen screen opens" "screenshots/$LABEL-screen-$screen.png"
  else
    fail "$screen screen opens" "sidebar item not found"
  fi
done
ui_press "Settings" >/dev/null 2>&1
for page in General Tools Safety Access Connections "Help and data"; do
  if ui_press "$page"; then
    sleep 1.5
    shot "$LABEL-settings-$(_slug "$page")" >/dev/null
    pass "Settings > $page opens"
  else
    fail "Settings > $page opens" "page link not found"
  fi
done

# ── A server with a secret in the keychain ────────────────────────────────
# Signed releases only: an unsigned local build has no keychain access group,
# so the data-protection keychain refuses it (errSecMissingEntitlement).
if [[ "${SIGNED:-1}" != "1" ]]; then
  untested "keychain secret storage, plain-text and prompt checks" "unsigned local build; the data-protection keychain needs Toolport's signed entitlements"
else
# ── A server with a secret in the keychain ────────────────────────────────
log "Adding a server with a keychain secret"
added=0
if ui_press "Servers" && ui_press "Add server" && ui_wait "Add MCP server" 15; then
  shot "$LABEL-add-server-empty" >/dev/null
  ui_fill "Server name" "Mock" &&
    ui_fill "npx" "$NODE" &&
    ui_fill "-y @scope/package" "$MOCK" &&
    ui_press "Add variable" &&
    ui_fill "ENV_NAME" "MOCK_API_TOKEN" &&
    ui_fill "value" "$SECRET" secure &&
    added=1
  shot "$LABEL-add-server-filled" >/dev/null
  if [[ $added -eq 1 ]] && ui_press "Add" && ui_gone "Add MCP server" 30; then
    sleep 2
    shot "$LABEL-add-server-done" >/dev/null
  else
    added=0
    shot "$LABEL-add-server-not-closed" >/dev/null
    tpui key 53 >/dev/null 2>&1 # Escape so later steps see the main window
  fi
fi
if [[ -s "$EVIDENCE/logs/ui-input-methods.txt" ]]; then
  info "text input method that reached the web view" "$(cut -f2 "$EVIDENCE/logs/ui-input-methods.txt" | sort | uniq -c | tr -s ' \n' ' ')"
fi
if [[ $added -eq 1 ]]; then
  pass "server form filled and submitted through the UI"
else
  fail "server form filled and submitted through the UI" "see logs/ax-fail-* and screenshots/fail-*"
fi
no_prompt_check "after storing the secret"

SERVER_ID="$(python3 - "$REGISTRY" "$NODE" <<'PY'
import json, sys
try:
    reg = json.load(open(sys.argv[1]))
except Exception:
    sys.exit(0)
for s in reg.get("servers", []):
    if s.get("command") == sys.argv[2]:
        print(s.get("id", ""))
        break
PY
)"
cp "$REGISTRY" "$EVIDENCE/logs/registry-$LABEL.json" 2>/dev/null || true
if [[ -n "$SERVER_ID" ]]; then
  echo "$SERVER_ID" >"$EVIDENCE/server-id"
  decl="$(python3 -c '
import json, sys
reg = json.load(open(sys.argv[1]))
s = [s for s in reg["servers"] if s.get("id") == sys.argv[2]][0]
print(json.dumps([e for e in s.get("env", []) if e.get("key") == "MOCK_API_TOKEN"]), "enabled=%s" % s.get("enabled"))' "$REGISTRY" "$SERVER_ID")"
  if grep -q '"secret": true' <<<"$decl" && ! grep -q '"value"' <<<"$decl"; then
    pass "registry declares the token as a keychain secret with no value" "$SERVER_ID: $decl"
  else
    fail "registry declares the token as a keychain secret with no value" "$SERVER_ID: $decl"
  fi
else
  fail "server saved to registry.json" "no server with command $NODE in $REGISTRY"
fi

# The token must not be on disk in plain text anywhere Toolport or a client writes.
# grep: 0 found, 1 searched and not found, 2 some files could not be read.
search=()
for p in "$HOME/Library" "$HOME/.claude.json" "$HOME/.claude" "$HOME/.cursor" "$HOME/.codex" "$HOME/.config" \
  "$HOME/.gemini" "$HOME/.copilot" "$HOME/.codeium" "$HOME/.kiro" "$HOME/Library/Application Support/Toolport"; do
  [[ -e "$p" ]] && search+=("$p")
done
grep -rIl -D skip --exclude-dir=Keychains -F -- "$SECRET" "${search[@]}" >"$EVIDENCE/logs/plain-text-hits-$LABEL.txt" 2>"$EVIDENCE/logs/plain-text-errors-$LABEL.txt"
rc=$?
unreadable="$(grep -c . "$EVIDENCE/logs/plain-text-errors-$LABEL.txt")"
others="$(grep -vcE 'Operation not permitted|Permission denied|No such file|Operation not supported on socket' "$EVIDENCE/logs/plain-text-errors-$LABEL.txt")"
if [[ $rc -eq 0 ]]; then
  fail "token is not on disk in plain text" "found in: $(head -5 "$EVIDENCE/logs/plain-text-hits-$LABEL.txt" | tr '\n' ' ')"
elif [[ $rc -eq 1 || ( $rc -eq 2 && $others -eq 0 ) ]]; then
  pass "token is not on disk in plain text" "searched ${#search[@]} locations (~/Library except Keychains, client config folders)"
  [[ $unreadable -eq 0 ]] || untested "token not in plain text in files the runner cannot read" "$unreadable paths protected by macOS privacy controls (logs/plain-text-errors-$LABEL.txt)"
else
  fail "token is not on disk in plain text" "grep exit $rc: $(head -3 "$EVIDENCE/logs/plain-text-errors-$LABEL.txt" | tr '\n' ' ')"
fi

# Toolport keeps server secrets in the data-protection keychain under its team
# access group, not in the file-based login keychain.
# security exits 44 (errSecItemNotFound) when the item is absent.
tmo 30 security find-generic-password -s conduit-mcp -a "${SERVER_ID:-x}::MOCK_API_TOKEN" >/dev/null 2>"$EVIDENCE/logs/login-keychain-lookup-$LABEL.err"
rc=$?
if [[ $rc -eq 44 ]]; then
  pass "token is not in the legacy login keychain" "expected: it lives in the data-protection keychain"
elif [[ $rc -eq 0 ]]; then
  fail "token is not in the legacy login keychain" "found conduit-mcp ${SERVER_ID}::MOCK_API_TOKEN in the login keychain"
else
  fail "token is not in the legacy login keychain" "lookup failed (exit $rc): $(head -c 200 "$EVIDENCE/logs/login-keychain-lookup-$LABEL.err")"
fi
security dump-keychain "$HOME/Library/Keychains/login.keychain-db" 2>/dev/null |
  awk '/^keychain:/{blk=$0; next} {blk=blk "\n" $0} /^    "svce"/ && tolower($0) ~ /conduit|toolport/ {print blk}' \
  >"$EVIDENCE/logs/login-keychain-toolport-items-$LABEL.txt"
info "note: Toolport items in the login keychain" "$(grep -c '"svce"' "$EVIDENCE/logs/login-keychain-toolport-items-$LABEL.txt" 2>/dev/null || echo 0) (logs/login-keychain-toolport-items-$LABEL.txt)"

log "Reading the token back from the gateway process"
tmo 60 "$GATEWAY" --selftest-secrets >"$EVIDENCE/logs/selftest-secrets-$LABEL.log" 2>&1
rc=$?
if [[ $rc -eq 0 ]] && grep -Eq "^OK +${SERVER_ID:-none} :: MOCK_API_TOKEN" "$EVIDENCE/logs/selftest-secrets-$LABEL.log"; then
  pass "gateway reads the token from the keychain without a prompt" "$(grep -E '^OK|selftest' "$EVIDENCE/logs/selftest-secrets-$LABEL.log" | tr '\n' ' ')"
elif [[ $rc -eq 142 ]]; then
  fail "gateway reads the token from the keychain without a prompt" "gateway --selftest-secrets hung for 60s (a keychain prompt would do this)"
else
  fail "gateway reads the token from the keychain without a prompt" "exit $rc: $(tr '\n' ' ' <"$EVIDENCE/logs/selftest-secrets-$LABEL.log" | cut -c1-300)"
fi
no_prompt_check "gateway keychain read"

fi

# ── Connect each client ───────────────────────────────────────────────────
log "Connecting clients"
first=1
while IFS=$'\t' read -r cid name; do
  if not_offered "$cid"; then
    untested "$name connected through the UI" "the previous release (${PREVIOUS_TAG:-?}) does not offer $name"
    continue
  fi
  ok=0
  if ui_press "Clients" && ui_press "$name" && ui_press "Connect to Toolport"; then
    if ui_wait "Review and connect $name" 20; then
      [[ $first -eq 1 ]] && shot "$LABEL-connect-review-$cid" >/dev/null
      if ui_press "Connect to Toolport" -1 && ui_wait "$name connected" 45; then
        ok=1
        [[ $first -eq 1 ]] && shot "$LABEL-connect-done-$cid" >/dev/null
        ui_press "Done" >/dev/null 2>&1 || true
      fi
    fi
  fi
  if [[ $ok -eq 1 ]]; then
    pass "$name connected through the UI"
  else
    fail "$name connected through the UI" "see logs/ax-fail-* and screenshots/fail-*"
    tpui key 53 >/dev/null 2>&1 || true # Escape any open dialog before the next client
  fi
  first=0
done <<<"$CLIENTS"
shot "$LABEL-clients-after-connect" >/dev/null
no_prompt_check "after connecting clients"

# ── Check the configs against each client's documented format ─────────────
log "Checking written client configs"
mkdir -p "$EVIDENCE/client-configs-$LABEL"
python3 -c 'import json,sys,shutil,os
for k, v in json.load(open(sys.argv[1])).items():
    if os.path.exists(v["path"]):
        shutil.copyfile(v["path"], os.path.join(sys.argv[2], k + os.path.splitext(v["path"])[1]))' "$MANIFEST" "$EVIDENCE/client-configs-$LABEL"
if [[ "$LABEL" == previous ]]; then
  # The previous release is not under test: its own config format is noted, and
  # the tool calls below still fail for any entry it wrote that does not work.
  record_rows "clients ($LABEL)" "client configs validated" python3 "$HERE/clients.py" check "$HOME" "$MANIFEST" "$ENTRIES" "--registry=$REGISTRY" "--previous=${PREVIOUS_TAG:-previous release}"
else
  record_rows "clients ($LABEL)" "client configs validated" python3 "$HERE/clients.py" check "$HOME" "$MANIFEST" "$ENTRIES" "--registry=$REGISTRY"
fi

# ── A real stdio tool call through each entry Toolport wrote ──────────────
log "Calling the mock server through each client entry"
# Every seeded client gets a call: a client with no usable entry is a FAIL here
# too, so a missing entry can never shrink the set of calls that must pass.
what="gets the keychain token"
[[ "$CALL_TOOL" == check_secret ]] || what="reaches Toolport"
while IFS=$'\t' read -r cid name; do
  entry="$ENTRIES/$cid.json"
  if not_offered "$cid"; then
    untested "tool call through the $cid entry $what" "the previous release (${PREVIOUS_TAG:-?}) does not offer $name"
    continue
  fi
  if [[ ! -f "$entry" ]]; then
    fail "tool call through the $cid entry $what" "no usable entry was written for $name"
    continue
  fi
  out="$EVIDENCE/logs/tool-call-$LABEL-$cid.json"
  if tool_call "$entry" "$CALL_TOOL" "$out"; then
    pass "tool call through the $cid entry $what" "$TOOL_CALL_DETAIL"
  else
    fail "tool call through the $cid entry $what" "$TOOL_CALL_DETAIL (logs/tool-call-$LABEL-$cid.json)"
  fi
done <<<"$CLIENTS"
if [[ -n "$(pids_of toolport-gateway)" ]]; then
  pass "gateway daemon runs after client calls" "$(ps -axo pid=,command= | grep toolport-gateway | grep -v grep | head -3 | sed 's/^ *//' | cut -c1-140 | tr '\n' ';')"
else
  fail "gateway daemon runs after client calls" "no toolport-gateway process"
fi
ui_press "Activity" >/dev/null 2>&1 && sleep 2
shot "$LABEL-activity-after-calls" >/dev/null
no_prompt_check "after tool calls"

# ── Launch at login ───────────────────────────────────────────────────────
# Off by default. Turn it on the way a user does and check the login item it
# creates; the uninstall steps then turn it off (or leave it on) and check.
log "Launch at login"
state="$(login_item_state)"
if [[ "$state" == absent ]]; then
  pass "Launch at login is off by default" "no $LOGIN_PLIST"
else
  fail "Launch at login is off by default" "$state"
fi
if set_launch_at_login on; then
  cp "$LOGIN_PLIST" "$EVIDENCE/logs/login-item-$LABEL.plist"
  pass "Launch at login creates a login item that starts Toolport" "$(login_item_state) ($LOGIN_PLIST)"
else
  fail "Launch at login creates a login item that starts Toolport" "$(login_item_state); see screenshots/fail-*"
fi
shot "$LABEL-launch-at-login-on" >/dev/null
if TPUI_TIMEOUT=20 tpui find "$(app_pid)" "Couldn't enable launch at login" >/dev/null 2>&1; then
  fail "Launch at login shows no error" "the app shows \"Couldn't enable launch at login\""
fi
finish_step
