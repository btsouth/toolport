#!/usr/bin/env bash
# Uninstall the way docs/uninstall.md says for macOS: disconnect every client
# while Toolport is installed (in Settings, or with the gateway CLI), turn off
# Launch at login, quit, and drag the app to the Trash. Optionally remove
# Toolport data first. Client configs must come back byte-exact.
#
# The UI run that keeps data also covers a user who skips turning Launch at
# login off: it turns it back on, starts the gateway daemon through a client
# entry after quitting, and expects the daemon to remove the LaunchAgent once
# the app is in the Trash. Without a running daemon nothing can clean it up.
#
# usage: uninstall.sh <label> <client-manifest.json> <ui|cli> [--remove-data]
set -uo pipefail
LABEL="$1"
MANIFEST="$2"
MODE="$3"
REMOVE_DATA=0
[[ "${4:-}" == "--remove-data" ]] && REMOVE_DATA=1
TRASH_LOGIN_CHECK=0
AREA="uninstall ($LABEL)"
HERE="$(cd "$(dirname "$0")" && pwd)"
# shellcheck source=common.sh
source "$HERE/common.sh"
# shellcheck source=ui.sh
source "$HERE/ui.sh"

ls -la "$HOME/Library/LaunchAgents" >"$EVIDENCE/logs/launchagents-before-uninstall-$LABEL.txt" 2>&1

if [[ "$MODE" == "ui" ]]; then
  [[ -n "$(app_pid)" ]] || { tmo 60 open "$APP_PATH" & wait_for 45 app_pid; sleep 5; }
  ui_activate
  if ui_press "Settings" && ui_press "Help and data" && ui_press "Remove Toolport from all clients" &&
    ui_wait "Remove Toolport from all clients?" 15; then
    shot "$LABEL-remove-from-clients-confirm" >/dev/null
    if ui_press "Remove from all clients" && sleep 6; then
      shot "$LABEL-remove-from-clients-result" >/dev/null
      pass "Remove Toolport from all clients (Settings)"
    else
      fail "Remove Toolport from all clients (Settings)" "confirm button not found"
    fi
  else
    fail "Remove Toolport from all clients (Settings)" "button or dialog not found"
  fi
  # docs/uninstall.md: turn off Launch at login before removing the app.
  before="$(login_item_state)"
  if [[ "$before" == absent ]]; then
    untested "turning off Launch at login removes the login item" "it was not on (setup could not enable it)"
  elif set_launch_at_login off; then
    pass "turning off Launch at login removes the login item" "$LOGIN_PLIST is gone"
  else
    fail "turning off Launch at login removes the login item" "still there: $(login_item_state)"
  fi
  # A data-removing run skips this: the client call below would recreate data.
  if [[ $REMOVE_DATA -eq 0 ]]; then
    if set_launch_at_login on; then
      TRASH_LOGIN_CHECK=1
      pass "Launch at login on again for the Trash check" "$(login_item_state)"
    else
      fail "Launch at login on again for the Trash check" "$(login_item_state); see screenshots/fail-*"
    fi
  fi
  shot "$LABEL-settings-general-before-uninstall" >/dev/null
else
  # Left on deliberately: this path records what a user who skips that step
  # is left with (listed by leftovers.sh).
  state="$(login_item_state)"
  info "note: Launch at login left on for this uninstall" "$state"
  [[ "$state" == absent ]] || cp "$LOGIN_PLIST" "$EVIDENCE/logs/login-item-left-on-$LABEL.plist"
  tmo 60 "$GATEWAY" --disconnect-all --dry-run >"$EVIDENCE/logs/disconnect-dry-run-$LABEL.json" 2>&1
  rc=$?
  if [[ $rc -eq 0 ]]; then pass "--disconnect-all --dry-run" "logs/disconnect-dry-run-$LABEL.json"
  else fail "--disconnect-all --dry-run" "exit $rc: $(tr -d '\n' <"$EVIDENCE/logs/disconnect-dry-run-$LABEL.json" | cut -c1-300)"; fi
  tmo 120 "$GATEWAY" --disconnect-all >"$EVIDENCE/logs/disconnect-all-$LABEL.json" 2>&1
  rc=$?
  if [[ $rc -eq 0 ]]; then
    pass "toolport-gateway --disconnect-all" "$(tr -d '\n' <"$EVIDENCE/logs/disconnect-all-$LABEL.json" | cut -c1-200)"
  else
    fail "toolport-gateway --disconnect-all" "exit $rc: $(tr -d '\n' <"$EVIDENCE/logs/disconnect-all-$LABEL.json" | cut -c1-300)"
  fi
fi

record_rows "uninstall ($LABEL)" "client configs compared after disconnecting" python3 "$HERE/clients.py" restored "$HOME" "$MANIFEST"

quit_check "app quits"
daemon_gone_check "no gateway daemon after quitting with no client connected" 15

# The daemon only runs while a client uses Toolport: start it the way a client
# does, through an entry setup wrote, with the app already quit.
start_daemon_via_client() { # start_daemon_via_client <check name> <log tag>
  local entry="" candidate out
  for candidate in "$EVIDENCE/client-entries-$LABEL/claude-code.json" "$EVIDENCE/client-entries-$LABEL"/*.json; do
    [[ -f "$candidate" ]] && { entry="$candidate"; break; }
  done
  out="$EVIDENCE/logs/tool-call-$2-$LABEL.json"
  if [[ -z "$entry" ]]; then
    fail "$1" "setup wrote no client entry"
  elif tool_call "$entry" toolport_status "$out" &&
    wait_for 10 bash -c "ps -axo command= | grep -F 'toolport-gateway --daemon' | grep -vq grep"; then
    pass "$1" "$(basename "$entry" .json) entry, $TOOL_CALL_DETAIL"
  else
    fail "$1" "${TOOL_CALL_DETAIL:-no call}; daemon: $(pids_of 'toolport-gateway --daemon' | tr '\n' ' ') (logs/$(basename "$out"))"
  fi
}

if [[ $TRASH_LOGIN_CHECK -eq 1 ]]; then
  # An updater moves the old bundle aside before the new one lands. The daemon
  # sees its binary gone and exits, but that is not a removal: the LaunchAgent
  # must survive and still start the app once it is back.
  start_daemon_via_client "a client call starts the daemon before the app is moved aside" aside
  aside="$(mktemp -d)/Toolport.app"
  if mv "$APP_PATH" "$aside"; then
    daemon_gone_check "the daemon leaves once its app is moved aside" 30
    if [[ -e "$LOGIN_PLIST" ]]; then
      pass "the LaunchAgent survives the app being moved aside" "$(login_item_state)"
    else
      fail "the LaunchAgent survives the app being moved aside" "removed; gateway.log: $(grep -h 'launch at login\|binary_removed' "$DATA_DIR"/gateway*.log 2>/dev/null | tail -3 | tr '\n' ' ' | cut -c1-300)"
    fi
    if mv "$aside" "$APP_PATH"; then
      pass "the app moved back into place" "$(login_item_state)"
    else
      fail "the app moved back into place" "mv $aside $APP_PATH failed"
    fi
  else
    fail "app moved aside like an updater does" "mv $APP_PATH $aside failed"
  fi
  start_daemon_via_client "a client call starts the daemon before the Trash" trash
fi

if [[ $REMOVE_DATA -eq 1 && "$MODE" == "ui" ]]; then
  # Settings > Help and data > Remove Toolport data closes the app and removes
  # data, then writes a report in the home folder.
  tmo 60 open "$APP_PATH" >/dev/null 2>&1 &
  wait_for 45 app_pid && sleep 6
  ui_activate
  if ui_press "Settings" && ui_press "Help and data" && ui_press "Remove Toolport data" &&
    ui_wait "Remove Toolport data?" 15; then
    shot "$LABEL-remove-data-confirm" >/dev/null
    ui_press "Close and remove data" || fail "Remove Toolport data confirmed" "\"Close and remove data\" not found"
    if wait_for 120 bash -c "! ps -axo command= | grep -F '/Contents/MacOS/$EXE' | grep -vq grep"; then
      pass "Remove Toolport data closes the app"
    else
      fail "Remove Toolport data closes the app" "still running after 120s"
      shot "$LABEL-remove-data-stuck" >/dev/null
    fi
    sleep 5
    report="$(ls -t "$HOME"/Toolport-removal-report*.json 2>/dev/null | head -1)"
    if [[ -n "$report" ]]; then
      cp "$report" "$EVIDENCE/logs/remove-data-report-$LABEL.json"
      left="$(python3 -c 'import json,sys; d=json.load(open(sys.argv[1])); print(len(d.get("leftovers") or []), json.dumps(d.get("leftovers"))[:300])' "$report" 2>&1)"
      if [[ "$left" == "0 "* ]]; then
        pass "Remove Toolport data completes" "report: $(basename "$report")"
      else
        fail "Remove Toolport data completes" "report lists leftovers: $left (logs/remove-data-report-$LABEL.json)"
      fi
    else
      fail "Remove Toolport data completes" "no removal report in the home folder"
    fi
  else
    fail "Remove Toolport data (Settings)" "button or dialog not found"
  fi
  no_prompt_check "remove data"
elif [[ $REMOVE_DATA -eq 1 ]]; then
  tmo 120 "$GATEWAY" --remove-data --dry-run >"$EVIDENCE/logs/remove-data-dry-run-$LABEL.json" 2>&1
  rc=$?
  if [[ $rc -eq 0 ]]; then pass "--remove-data --dry-run" "logs/remove-data-dry-run-$LABEL.json"
  else fail "--remove-data --dry-run" "exit $rc: $(tr -d '\n' <"$EVIDENCE/logs/remove-data-dry-run-$LABEL.json" | cut -c1-300)"; fi
  tmo 300 "$GATEWAY" --remove-data --confirm >"$EVIDENCE/logs/remove-data-$LABEL.json" 2>&1
  rc=$?
  if [[ $rc -eq 0 ]]; then
    pass "toolport-gateway --remove-data --confirm" "exit 0 (logs/remove-data-$LABEL.json)"
  else
    fail "toolport-gateway --remove-data --confirm" "exit $rc: $(tr -d '\n' <"$EVIDENCE/logs/remove-data-$LABEL.json" | cut -c1-300)"
  fi
  no_prompt_check "remove data"
fi

# Drag to Trash.
mkdir -p "$HOME/.Trash"
rm -rf "$HOME/.Trash/Toolport.app"
if mv "$APP_PATH" "$HOME/.Trash/Toolport.app"; then pass "app moved to the Trash"; else fail "app moved to the Trash"; fi
if [[ $TRASH_LOGIN_CHECK -eq 1 ]]; then
  # The daemon notices its app is gone, then removes the LaunchAgent that
  # would start it at every login, and unloads it.
  if wait_for 30 test ! -e "$LOGIN_PLIST"; then
    pass "no LaunchAgent left after the Trash with Launch at login on" "the daemon removed $LOGIN_PLIST"
  else
    fail "no LaunchAgent left after the Trash with Launch at login on" "$(login_item_state) after 30s; gateway.log: $(grep -h 'launch at login\|binary_removed\|was removed or replaced' "$DATA_DIR"/gateway*.log 2>/dev/null | tail -3 | tr '\n' ' ' | cut -c1-300)"
  fi
  if launchctl print "gui/$(id -u)/Toolport" >/dev/null 2>&1; then
    fail "the Toolport LaunchAgent is not loaded after the Trash" "launchctl print gui/$(id -u)/Toolport still finds it"
  else
    pass "the Toolport LaunchAgent is not loaded after the Trash"
  fi
fi
sleep 5
daemon_gone_check "no gateway daemon after the app went to the Trash" 15
finish_step
