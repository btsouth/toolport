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
SECRET_SHA="sha256:$(printf '%s' "$SECRET" | shasum -a 256 | cut -d' ' -f1)"
NODE="$(command -v node)"
MOCK_DIR="$HOME/mcp-servers"
MOCK="$MOCK_DIR/mock-mcp.mjs"
mkdir -p "$MOCK_DIR"
cp "$HERE/mock-mcp.mjs" "$MOCK"
REGISTRY="$DATA_DIR/registry.json"
ENTRIES="$EVIDENCE/client-entries-$LABEL"

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
  while IFS=$'\t' read -r _ name; do
    if ui_wait "$name" 3; then detected+="$name, "; else missing+="$name, "; fi
  done < <(python3 -c 'import json,sys; [print(k + "\t" + v["name"]) for k, v in json.load(open(sys.argv[1])).items()]' "$MANIFEST")
  if [[ -z "$missing" ]]; then
    pass "onboarding lists every seeded client" "${detected%, }"
  else
    fail "onboarding lists every seeded client" "missing: ${missing%, } (listed: ${detected%, })"
  fi
  ui_press_any 10 "Skip for now" "Next" >/dev/null && shot "$LABEL-onboarding-4-done" >/dev/null
  if ui_press_any 60 "Start using Toolport" "Got it" "Continue without verification" >/dev/null; then
    pass "onboarding can be completed"
  else
    fail "onboarding can be completed" "no finish button found"
  fi
else
  info "onboarding" "not shown on this launch"
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
log "Adding a server with a keychain secret"
added=0
if ui_press "Servers" && ui_press "Add server" && ui_wait "Add MCP server" 15; then
  shot "$LABEL-add-server-empty" >/dev/null
  ui_fill "Server name" "Mock" &&
    ui_fill "npx" "$NODE" &&
    ui_fill "-y @scope/package" "$MOCK" &&
    ui_press "Add variable" &&
    ui_fill "ENV_NAME" "MOCK_API_TOKEN" &&
    ui_fill "value" "$SECRET" &&
    added=1
  shot "$LABEL-add-server-filled" >/dev/null
  if [[ $added -eq 1 ]] && ui_press "Add"; then
    sleep 4
    shot "$LABEL-add-server-done" >/dev/null
  else
    added=0
  fi
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
plain="$(grep -rIl --exclude-dir=Keychains -F -- "$SECRET" "$HOME/Library" "$HOME/.claude.json" "$HOME/.cursor" \
  "$HOME/.codex" "$HOME/.config" "$HOME/.gemini" "$HOME/.copilot" "$HOME/.codeium" "$HOME/.kiro" 2>/dev/null | head -5)"
if [[ -z "$plain" ]]; then
  pass "token is not on disk in plain text" "searched ~/Library (except Keychains) and client config folders"
else
  fail "token is not on disk in plain text" "found in: $plain"
fi

# Toolport keeps server secrets in the data-protection keychain under its team
# access group, not in the file-based login keychain.
if security find-generic-password -s conduit-mcp -a "${SERVER_ID:-x}::MOCK_API_TOKEN" >/dev/null 2>&1; then
  fail "token is not in the legacy login keychain" "found conduit-mcp ${SERVER_ID}::MOCK_API_TOKEN in the login keychain"
else
  pass "token is not in the legacy login keychain" "expected: it lives in the data-protection keychain"
fi
security dump-keychain "$HOME/Library/Keychains/login.keychain-db" 2>/dev/null |
  awk '/^keychain:/{blk=$0; next} {blk=blk "\n" $0} /^    "svce"/ && /conduit|toolport/i {print blk}' \
  >"$EVIDENCE/logs/login-keychain-toolport-items-$LABEL.txt" || true
info "Toolport items in the login keychain" "$(grep -c '"svce"' "$EVIDENCE/logs/login-keychain-toolport-items-$LABEL.txt" 2>/dev/null || echo 0) (logs/login-keychain-toolport-items-$LABEL.txt)"

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

# ── Connect each client ───────────────────────────────────────────────────
log "Connecting clients"
first=1
while IFS=$'\t' read -r cid name; do
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
done < <(python3 -c 'import json,sys; [print(k + "\t" + v["name"]) for k, v in json.load(open(sys.argv[1])).items()]' "$MANIFEST")
shot "$LABEL-clients-after-connect" >/dev/null
no_prompt_check "after connecting clients"

# ── Check the configs against each client's documented format ─────────────
log "Checking written client configs"
mkdir -p "$EVIDENCE/client-configs-$LABEL"
python3 -c 'import json,sys,shutil,os
for k, v in json.load(open(sys.argv[1])).items():
    if os.path.exists(v["path"]):
        shutil.copyfile(v["path"], os.path.join(sys.argv[2], k + os.path.splitext(v["path"])[1]))' "$MANIFEST" "$EVIDENCE/client-configs-$LABEL"
python3 "$HERE/clients.py" check "$HOME" "$MANIFEST" "$ENTRIES" | sed "s/^clients\t/clients ($LABEL)\t/" >>"$RESULTS"
grep -q $'\tFAIL\t' <(grep "^clients ($LABEL)" "$RESULTS") && STEP_FAILED=1

# ── A real stdio tool call through each entry Toolport wrote ──────────────
log "Calling the mock server through each client entry"
for entry in "$ENTRIES"/*.json; do
  [[ -e "$entry" ]] || continue
  cid="$(basename "$entry" .json)"
  out="$EVIDENCE/logs/tool-call-$LABEL-$cid.json"
  MCP_CALL_TIMEOUT_MS=90000 node "$HERE/mcp-call.mjs" "$entry" check_secret >"$out" 2>&1 &
  call=$!
  approved="$(approve_pending 95 "$call")"
  wait "$call"
  rc=$?
  result="$(python3 -c 'import json,sys; d=json.load(open(sys.argv[1])); print(d.get("result",""), "|", d.get("calledVia",""), "|", d.get("detail",""))' "$out" 2>/dev/null)"
  if [[ $rc -eq 0 ]] && grep -qF "$SECRET_SHA" "$out"; then
    pass "tool call through the $cid entry gets the keychain token" "${result} approvals=$approved"
  else
    fail "tool call through the $cid entry gets the keychain token" "exit $rc: ${result:-$(tail -c 300 "$out")} approvals=$approved (logs/tool-call-$LABEL-$cid.json)"
  fi
done
shot "$LABEL-activity-after-calls" >/dev/null || true
no_prompt_check "after tool calls"
finish_step
