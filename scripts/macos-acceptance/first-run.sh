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
CRASH_DIR="$HOME/Library/Logs/DiagnosticReports"
# Crash reports for Toolport; exits non-zero only when the folder exists but
# cannot be listed (a missing folder means no reports yet).
crash_list() {
  [[ -d "$CRASH_DIR" ]] || return 0
  local all
  all="$(ls -1 "$CRASH_DIR")" || return 1
  grep -iE 'toolport|conduit' <<<"$all" || true
}
windows_json() { TPUI_TIMEOUT=15 tpui windows; }

crash_probe_ok=1
crashes_before="$(crash_list)" || crash_probe_ok=0
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
  gk_probe_errors=0
  for _ in $(seq 1 25); do
    gk_pid="$(winq gatekeeper)"
    rc=$?
    [[ $rc -eq 0 ]] && break
    [[ $rc -ge 2 ]] && gk_probe_errors=$((gk_probe_errors + 1))
    gk_pid=""
    [[ -n "$(app_pid)" ]] && winq owner Toolport >/dev/null && break
    sleep 1
  done
  if [[ -n "$gk_pid" ]]; then
    shot "$LABEL-gatekeeper-prompt" >/dev/null || true
    pass "Gatekeeper shows its first-open prompt for the quarantined app" "screenshots/$LABEL-gatekeeper-prompt.png"
    # The same agent first shows a "Verifying" progress window, then the
    # prompt; keep trying Open while that agent has a window (up to 120s).
    pressed=1
    for _ in $(seq 1 120); do
      if TPUI_TIMEOUT=20 tpui press "$gk_pid" "Open" >"$EVIDENCE/logs/gatekeeper-press-$LABEL.log" 2>&1; then pressed=0; break; fi
      winq gatekeeper >/dev/null || break
      sleep 1
    done
    tpui dump "$gk_pid" 12 >"$EVIDENCE/logs/ax-gatekeeper-$LABEL.txt" 2>&1
    shot "$LABEL-gatekeeper-after-press" >/dev/null
    if [[ $pressed -eq 0 ]]; then
      pass "Gatekeeper prompt accepted through accessibility" "$(cat "$EVIDENCE/logs/gatekeeper-press-$LABEL.log")"
    else
      fail "Gatekeeper prompt accepted through accessibility" "$(tail -n 2 "$EVIDENCE/logs/gatekeeper-press-$LABEL.log" | tr '\n' ' ')"
    fi
  elif [[ $gk_probe_errors -gt 0 ]]; then
    fail "Gatekeeper shows its first-open prompt for the quarantined app" "the window list could not be read $gk_probe_errors times while looking for it"
  else
    untested "Gatekeeper shows its first-open prompt for the quarantined app" "no prompt appeared and the app opened directly; macOS did not treat this copy as a first open"
  fi
else
  untested "Gatekeeper first-open prompt" "the installed app carries no quarantine attribute (Homebrew or local install path)"
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
has_window() { winq app-window "$PID" >/dev/null; }
log "Waiting for the main window"
if wait_for 45 has_window; then
  pass "main window appears" "$(winq app-window "$PID")"
else
  fail "main window appears" "no on-screen window for pid $PID after 45s (window probe: $(winq app-window "$PID" 2>&1; echo "exit $?"))"
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
# macOS 26 hosts status items in Control Center's windows, so ask the app's
# own accessibility extras menu bar instead of CGWindowList.
if tpui menubar "$PID" >"$EVIDENCE/logs/ax-menubar-$LABEL.txt" 2>&1; then
  pass "menu bar item exists" "$(head -n 1 "$EVIDENCE/logs/ax-menubar-$LABEL.txt")"
  xy="$(sed -n 's/.*@\(-\{0,1\}[0-9]*\),\([0-9]*\).*/\1 \2/p' "$EVIDENCE/logs/ax-menubar-$LABEL.txt" | head -1)"
  if [[ -n "$xy" ]]; then
    tpui click $((${xy% *} + 12)) $((${xy#* } + 12)) >/dev/null 2>&1
    sleep 1.5
    shot "$LABEL-tray-menu" >/dev/null || true
    if TPUI_MENUS=1 tpui find "$PID" "Quit Toolport" >"$EVIDENCE/logs/tray-menu-$LABEL.txt" 2>&1; then
      pass "menu bar item opens its menu" "has Quit Toolport (screenshots/$LABEL-tray-menu.png)"
    else
      fail "menu bar item opens its menu" "no Quit Toolport item after clicking it (screenshots/$LABEL-tray-menu.png)"
    fi
    # Close the menu by clicking its item again, only while it is open. An
    # Escape would reach the window once the menu is gone and dismiss the
    # onboarding dialog the setup step checks next.
    if TPUI_MENUS=1 tpui find "$PID" "Quit Toolport" >/dev/null 2>&1; then
      tpui click $((${xy% *} + 12)) $((${xy#* } + 12)) >/dev/null 2>&1
      sleep 1
      TPUI_MENUS=1 tpui find "$PID" "Quit Toolport" >/dev/null 2>&1 && info "note: tray menu" "still open after clicking its item again"
    fi
  else
    fail "menu bar item opens its menu" "could not read the item's position from $(head -n 1 "$EVIDENCE/logs/ax-menubar-$LABEL.txt")"
  fi
else
  fail "menu bar item exists" "$(tr '\n' ' ' <"$EVIDENCE/logs/ax-menubar-$LABEL.txt")"
fi

# The gateway daemon starts when a client first connects, not with the app.
if [[ -n "$(pids_of toolport-gateway)" ]]; then
  info "gateway process at first launch" "$(ps -axo pid=,command= | grep toolport-gateway | grep -v grep | head -3 | cut -c1-160 | tr '\n' ';')"
else
  info "gateway process at first launch" "none yet (it starts with the first client connection)"
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
crashes_after="$(crash_list)" || crash_probe_ok=0
if [[ $crash_probe_ok -eq 0 ]]; then
  fail "no crash report written" "$CRASH_DIR exists but could not be listed"
  finish_step
fi
new_crashes="$(comm -13 <(printf '%s\n' "$crashes_before" | sort) <(printf '%s\n' "$crashes_after" | sort) | grep -v '^$' || true)"
if [[ -z "$new_crashes" ]]; then
  pass "no crash report written"
else
  fail "no crash report written" "$(printf '%s ' $new_crashes)"
fi
if [[ -n "$(app_pid)" ]]; then
  pass "app still running after first launch"
else
  fail "app still running after first launch" "process $PID exited"
fi
finish_step
