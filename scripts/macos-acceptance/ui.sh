# shellcheck shell=bash
# Accessibility helpers that drive the running Toolport window the way a user
# clicks and types. Source after common.sh. Every helper waits for its target,
# and on failure saves the accessibility tree and a screenshot as evidence.

EXE="$(/usr/libexec/PlistBuddy -c 'Print :CFBundleExecutable' "$APP_PATH/Contents/Info.plist" 2>/dev/null || echo conduit)"
EXE_PATH="$APP_PATH/Contents/MacOS/$EXE"
app_pid() { pids_of "/Contents/MacOS/$EXE" | head -n 1; }
UI_FAILS=0

_slug() { printf '%s' "$*" | tr -c 'A-Za-z0-9._-' '_' | cut -c1-60; }

ui_evidence() {
  local why="$1" pid
  pid="$(app_pid)"
  UI_FAILS=$((UI_FAILS + 1))
  [[ -n "$pid" ]] && tpui dump "$pid" 60 >"$EVIDENCE/logs/ax-fail-$UI_FAILS-$(_slug "$why").txt" 2>&1
  shot "fail-$UI_FAILS-$(_slug "$why")" >/dev/null || true
}

ui_activate() {
  tmo 15 osascript -e 'tell application id "com.tsout.conduit" to activate' >/dev/null 2>&1 || true
  sleep 0.5
}

# ui_wait <text> [seconds]: wait until an element labelled <text> exists.
ui_wait() {
  local text="$1" secs="${2:-20}" i pid
  for ((i = 0; i < secs; i++)); do
    pid="$(app_pid)"
    if [[ -n "$pid" ]] && TPUI_TIMEOUT=20 tpui find "$pid" "$text" >/dev/null 2>&1; then
      return 0
    fi
    sleep 1
  done
  return 1
}

# ui_press <text> [n] [seconds]: press the n-th control labelled <text>.
ui_press() {
  local text="$1" n="${2:-1}" secs="${3:-20}" i pid out
  for ((i = 0; i < secs; i++)); do
    pid="$(app_pid)"
    if [[ -n "$pid" ]] && out="$(TPUI_TIMEOUT=30 tpui press "$pid" "$text" "$n" 2>&1)"; then
      printf '[ui] %s\n' "$out" >&2
      sleep 0.8
      return 0
    fi
    sleep 1
  done
  printf '[ui] could not press "%s": %s\n' "$text" "${out:-no app process}" >&2
  ui_evidence "press $text"
  return 1
}

# ui_press_any <seconds> <text>...: press the first label that appears.
ui_press_any() {
  local secs="$1" i t pid out
  shift
  for ((i = 0; i < secs; i++)); do
    pid="$(app_pid)"
    for t in "$@"; do
      if [[ -n "$pid" ]] && out="$(TPUI_TIMEOUT=30 tpui press "$pid" "$t" 1 2>&1)"; then
        printf '[ui] %s\n' "$out" >&2
        sleep 0.8
        printf '%s\n' "$t"
        return 0
      fi
    done
    sleep 1
  done
  ui_evidence "press any $1"
  return 1
}

# ui_fill <field-label> <text> [n]: focus a text field, clear it, type <text>.
ui_fill() {
  local field="$1" text="$2" n="${3:-1}" i pid out
  for ((i = 0; i < 20; i++)); do
    pid="$(app_pid)"
    if [[ -n "$pid" ]] && out="$(TPUI_TIMEOUT=30 tpui focus "$pid" "$field" "$n" 2>&1)"; then
      printf '[ui] %s\n' "$out" >&2
      sleep 0.3
      tpui key 0 cmd   # select all
      tpui key 51      # delete
      tpui keys "$text"
      sleep 0.3
      return 0
    fi
    sleep 1
  done
  printf '[ui] could not focus "%s": %s\n' "$field" "${out:-no app process}" >&2
  ui_evidence "fill $field"
  return 1
}

# A keychain or other system authorization prompt on screen. Any such window
# would block a real user and hang an unattended run.
system_prompt_visible() {
  TPUI_TIMEOUT=15 tpui windows | python3 -c '
import json, sys
hits = [w for w in json.load(sys.stdin)
        if w["owner"] in ("SecurityAgent", "UserNotificationCenter", "universalAccessAuthWarn", "coreautha")
        and w["onscreen"] and w["w"] > 50]
for w in hits:
    print("%s %s %dx%d" % (w["owner"], w["name"], w["w"], w["h"]))
sys.exit(0 if hits else 1)'
}

# no_prompt_check <label>: record whether a system prompt is on screen now.
no_prompt_check() {
  local what
  if what="$(system_prompt_visible)"; then
    shot "prompt-$(_slug "$1")" >/dev/null || true
    fail "no keychain or system prompt ($1)" "$what (screenshots/prompt-$(_slug "$1").png)"
    return 1
  fi
  pass "no keychain or system prompt ($1)"
}

# approve_pending <seconds> <pid-of-call>: while a tool call runs, press
# Approve if Toolport holds it for approval. Prints how many were approved.
approve_pending() {
  local secs="$1" call_pid="$2" i approved=0 pid
  for ((i = 0; i < secs; i++)); do
    kill -0 "$call_pid" 2>/dev/null || break
    pid="$(app_pid)"
    if [[ -n "$pid" ]] && TPUI_TIMEOUT=20 tpui press "$pid" "Approve" 1 >/dev/null 2>&1; then
      approved=$((approved + 1))
      shot "approval-$approved" >/dev/null || true
    fi
    sleep 1
  done
  printf '%s\n' "$approved"
}

quit_app() {
  local pid
  pid="$(app_pid)"
  [[ -z "$pid" ]] && return 0
  tmo 20 osascript -e 'tell application id "com.tsout.conduit" to quit' >/dev/null 2>&1 || true
  if ! wait_for 20 bash -c "! ps -p $pid >/dev/null"; then
    kill "$pid" 2>/dev/null || true
    wait_for 10 bash -c "! ps -p $pid >/dev/null" || kill -9 "$pid" 2>/dev/null || true
    return 1
  fi
}
