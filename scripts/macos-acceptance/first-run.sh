#!/usr/bin/env bash
# First launch of the installed app, the way a new user opens it from
# Applications: Gatekeeper's first-open check for a quarantined download, the
# main window, the menu bar item, the gateway process, crash reports, and how
# far UI automation reaches on this runner.
#
# usage: first-run.sh [label]
set -uo pipefail
AREA="first run${1:+ ($1)}"
LABEL="${1:-first}"
# shellcheck source=common.sh
source "$(dirname "$0")/common.sh"

EXE="$(/usr/libexec/PlistBuddy -c 'Print :CFBundleExecutable' "$APP_PATH/Contents/Info.plist")"
EXE_PATH="$APP_PATH/Contents/MacOS/$EXE"
# Match any path: a quarantined app copied without Finder runs from a random
# App Translocation folder, which is itself worth reporting.
app_pid() { pids_of "/Contents/MacOS/$EXE" | head -n 1; }
crash_list() { ls -1 "$HOME/Library/Logs/DiagnosticReports" 2>/dev/null | grep -iE 'toolport|conduit' || true; }
windows_json() { TPUI_TIMEOUT=15 tpui windows; }

crashes_before="$(crash_list)"
tpui trust >"$EVIDENCE/logs/tcc-$LABEL.json" 2>&1
info "automation grants for the job's processes" "$(tr -d ' \n' <"$EVIDENCE/logs/tcc-$LABEL.json")"

quarantine="$(xattr -p com.apple.quarantine "$APP_PATH" 2>/dev/null || true)"
log "Opening $APP_PATH (quarantine: ${quarantine:-none})"
# open waits while Gatekeeper's confirmation is on screen, so run it in the
# background and handle the prompt below, as a user would.
tmo 120 open "$APP_PATH" >"$EVIDENCE/logs/open-$LABEL.log" 2>&1 &

# A quarantined, notarized download gets Gatekeeper's "downloaded from the
# Internet" confirmation before the app's own process starts.
gk_pid=""
if [[ -n "$quarantine" ]]; then
  log "Looking for the Gatekeeper prompt"
  # The app's process can exist while the prompt holds it, so look for the
  # prompt itself until the app shows a window.
  for _ in $(seq 1 25); do
    gk_pid="$(windows_json | python3 -c '
import json, sys
for w in json.load(sys.stdin):
    if w["owner"] in ("CoreServicesUIAgent", "UserNotificationCenter") and w["layer"] >= 0 and w["w"] > 100:
        print(w["pid"]); break' 2>/dev/null)"
    [[ -n "$gk_pid" ]] && break
    [[ -n "$(app_pid)" ]] && TPUI_TIMEOUT=15 tpui windows | grep -q '"owner" : "Toolport"' && break
    sleep 1
  done
  if [[ -n "$gk_pid" ]]; then
    shot "$LABEL-gatekeeper-prompt" >/dev/null && info "Gatekeeper first-open prompt" "shown (screenshots/$LABEL-gatekeeper-prompt.png)"
    tpui dump "$gk_pid" 12 >"$EVIDENCE/logs/ax-gatekeeper-$LABEL.txt" 2>&1
    if tpui press "$gk_pid" "Open" >"$EVIDENCE/logs/gatekeeper-press-$LABEL.log" 2>&1; then
      pass "Gatekeeper prompt accepted through accessibility" "$(cat "$EVIDENCE/logs/gatekeeper-press-$LABEL.log")"
    else
      fail "Gatekeeper prompt accepted through accessibility" "$(tail -n 2 "$EVIDENCE/logs/gatekeeper-press-$LABEL.log" | tr '\n' ' ')"
    fi
  elif [[ -n "$(app_pid)" ]]; then
    info "Gatekeeper first-open prompt" "none appeared; the app started directly"
  fi
fi

log "Waiting for the app process"
if wait_for 45 app_pid; then
  pass "app process starts" "pid $(app_pid)"
else
  fail "app process starts" "no $EXE process 45s after open"
  shot "$LABEL-no-process" >/dev/null || true
  finish_step
fi
PID="$(app_pid)"
echo "$PID" >"$EVIDENCE/app.pid"
running_from="$(ps -o command= -p "$PID" | cut -c1-300)"
if [[ "$running_from" == "$EXE_PATH"* ]]; then
  pass "app runs from $APP_PATH" "$running_from"
else
  fail "app runs from $APP_PATH" "running from $running_from (App Translocation means Gatekeeper still treats it as unmoved; client configs would get this temporary path)"
fi

# The window is created hidden and shown once the UI loads.
has_window() {
  windows_json | python3 -c '
import json, sys
pid = int(sys.argv[1])
ok = [w for w in json.load(sys.stdin) if w["pid"] == pid and w["layer"] == 0 and w["onscreen"] and w["w"] >= 400]
sys.exit(0 if ok else 1)' "$PID"
}
log "Waiting for the main window"
if wait_for 45 has_window; then
  pass "main window appears" "$(windows_json | python3 -c '
import json, sys
pid = int(sys.argv[1])
for w in json.load(sys.stdin):
    if w["pid"] == pid and w["layer"] == 0 and w["onscreen"]:
        print("%s %dx%d at %d,%d" % (w["name"] or "(title hidden without screen recording)", w["w"], w["h"], w["x"], w["y"]))' "$PID" | head -1)"
else
  fail "main window appears" "no on-screen window for pid $PID after 45s"
fi
windows_json >"$EVIDENCE/logs/windows-$LABEL.json"
sleep 3
if shot "$LABEL-window" >/dev/null; then
  pass "screenshot of the first window" "screenshots/$LABEL-window.png"
else
  fail "screenshot of the first window" "$(cat "$EVIDENCE/logs/screencapture-$LABEL-window.err")"
fi

# Menu bar status item: a layer-25 window owned by the app in CGWindowList, and
# the AX extras menu bar when accessibility is available.
log "Looking for the menu bar item"
tray="$(windows_json | python3 -c '
import json, sys
pid = int(sys.argv[1])
for w in json.load(sys.stdin):
    if w["pid"] == pid and w["layer"] == 25:
        print("%dx%d at %d,%d" % (w["w"], w["h"], w["x"], w["y"]))' "$PID" | head -1)"
if [[ -n "$tray" ]]; then
  pass "menu bar item exists" "status item window $tray"
else
  fail "menu bar item exists" "no layer-25 window owned by pid $PID"
fi
tpui menubar "$PID" >"$EVIDENCE/logs/ax-menubar-$LABEL.txt" 2>&1 || true

gateway_running() { [[ -n "$(pids_of toolport-gateway)" ]]; }
if wait_for 30 gateway_running; then
  pass "gateway process starts" "$(ps -axo pid=,command= | grep toolport-gateway | grep -v grep | head -3 | cut -c1-160 | tr '\n' ';')"
else
  fail "gateway process starts" "no toolport-gateway process 30s after launch"
fi
ps -axo pid,ppid,user,command | grep -iE 'toolport|conduit' | grep -v grep >"$EVIDENCE/logs/processes-$LABEL.txt" || true

log "Dumping the accessibility tree"
# How far accessibility reaches into the window (a WKWebView).
if tpui dump "$PID" 40 >"$EVIDENCE/logs/ax-tree-$LABEL.txt" 2>"$EVIDENCE/logs/ax-tree-$LABEL.err"; then
  n="$(wc -l <"$EVIDENCE/logs/ax-tree-$LABEL.txt" | tr -d ' ')"
  web="$(grep -c 'AXWebArea' "$EVIDENCE/logs/ax-tree-$LABEL.txt" || true)"
  buttons="$(grep -c 'AXButton' "$EVIDENCE/logs/ax-tree-$LABEL.txt" || true)"
  info "accessibility tree" "$n elements, $web web areas, $buttons buttons (logs/ax-tree-$LABEL.txt)"
else
  info "accessibility tree" "unavailable: $(cat "$EVIDENCE/logs/ax-tree-$LABEL.err")"
fi

sleep 5
crashes_after="$(crash_list)"
new_crashes="$(comm -13 <(printf '%s\n' "$crashes_before" | sort) <(printf '%s\n' "$crashes_after" | sort) | grep -v '^$' || true)"
if [[ -z "$new_crashes" ]]; then
  pass "no crash report written"
else
  fail "no crash report written" "$(printf '%s ' $new_crashes)"
fi
if [[ -z "$(app_pid)" ]]; then
  fail "app still running after first launch" "process $PID exited"
fi
finish_step
