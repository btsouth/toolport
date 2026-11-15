#!/usr/bin/env bash
# Uninstall the way docs/uninstall.md says for macOS: disconnect every client
# while Toolport is installed (in Settings, or with the gateway CLI), turn off
# Launch at login, quit, and drag the app to the Trash. Optionally remove
# Toolport data first. Client configs must come back byte-exact.
#
# usage: uninstall.sh <label> <client-manifest.json> <ui|cli> [--remove-data]
set -uo pipefail
LABEL="$1"
MANIFEST="$2"
MODE="$3"
REMOVE_DATA=0
[[ "${4:-}" == "--remove-data" ]] && REMOVE_DATA=1
AREA="uninstall ($LABEL)"
HERE="$(cd "$(dirname "$0")" && pwd)"
# shellcheck source=common.sh
source "$HERE/common.sh"
# shellcheck source=ui.sh
source "$HERE/ui.sh"

ls -la "$HOME/Library/LaunchAgents" >"$EVIDENCE/logs/launchagents-before-uninstall-$LABEL.txt" 2>&1 || true

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
  if ui_press "General"; then
    shot "$LABEL-settings-general-before-uninstall" >/dev/null
    tpui find "$(app_pid)" "Launch at login" >"$EVIDENCE/logs/launch-at-login-$LABEL.txt" 2>&1 || true
    info "Launch at login control" "$(head -n 3 "$EVIDENCE/logs/launch-at-login-$LABEL.txt" | tr '\n' ' ')"
  fi
else
  tmo 60 "$GATEWAY" --disconnect-all --dry-run >"$EVIDENCE/logs/disconnect-dry-run-$LABEL.json" 2>&1
  info "--disconnect-all --dry-run" "exit $? (logs/disconnect-dry-run-$LABEL.json)"
  if tmo 120 "$GATEWAY" --disconnect-all >"$EVIDENCE/logs/disconnect-all-$LABEL.json" 2>&1; then
    pass "toolport-gateway --disconnect-all" "$(tr -d '\n' <"$EVIDENCE/logs/disconnect-all-$LABEL.json" | cut -c1-200)"
  else
    fail "toolport-gateway --disconnect-all" "exit $?: $(tr -d '\n' <"$EVIDENCE/logs/disconnect-all-$LABEL.json" | cut -c1-300)"
  fi
fi

python3 "$HERE/clients.py" restored "$HOME" "$MANIFEST" | sed "s/^clients\t/uninstall ($LABEL)\t/" >>"$RESULTS"
grep "^uninstall ($LABEL)" "$RESULTS" | grep -q $'\tFAIL\t' && STEP_FAILED=1

if quit_app; then pass "app quits"; else fail "app quits" "had to be killed"; fi

if [[ $REMOVE_DATA -eq 1 && "$MODE" == "ui" ]]; then
  # Settings > Help and data > Remove Toolport data closes the app and removes
  # data, then writes a report in the home folder.
  tmo 60 open "$APP_PATH" >/dev/null 2>&1 &
  wait_for 45 app_pid && sleep 6
  ui_activate
  if ui_press "Settings" && ui_press "Help and data" && ui_press "Remove Toolport data" &&
    ui_wait "Remove Toolport data?" 15; then
    shot "$LABEL-remove-data-confirm" >/dev/null
    ui_press "Close and remove data" || true
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
      if [[ "${left%% *}" == "0" ]]; then
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
  info "--remove-data --dry-run" "exit $? (logs/remove-data-dry-run-$LABEL.json)"
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
sleep 5
finish_step
