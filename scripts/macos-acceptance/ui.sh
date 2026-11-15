# shellcheck shell=bash
# Accessibility helpers that drive the running Toolport window the way a user
# clicks and types. Source after common.sh. Every helper waits for its target,
# and on failure saves the accessibility tree and a screenshot as evidence.

_UI_HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
EXE="$(/usr/libexec/PlistBuddy -c 'Print :CFBundleExecutable' "$APP_PATH/Contents/Info.plist" 2>/dev/null || echo conduit)"
EXE_PATH="$APP_PATH/Contents/MacOS/$EXE"
# The app's own process: the executable path (ps comm) must end with
# /Contents/MacOS/<exe> exactly, so helpers such as conduit-gateway or
# toolport-gateway never match.
app_pid() {
  ps -axo pid=,comm= | awk -v s="/Contents/MacOS/$EXE" '{
    pid = $1; sub(/^ *[0-9]+ +/, "")
    if (length($0) >= length(s) && substr($0, length($0) - length(s) + 1) == s) { print pid; exit }
  }'
}
UI_FAILS=0

_slug() { printf '%s' "$*" | tr -c 'A-Za-z0-9._-' '_' | cut -c1-60; }

ui_evidence() {
  local why="$1" pid
  pid="$(app_pid)"
  UI_FAILS=$((UI_FAILS + 1))
  [[ -n "$pid" ]] && tpui dump "$pid" 60 >"$EVIDENCE/logs/ax-fail-$UI_FAILS-$(_slug "$why").txt" 2>&1
  shot "fail-$UI_FAILS-$(_slug "$why")" >/dev/null || true
}

# Bring the window forward and wait until its web content is exposed to
# accessibility (the sidebar's Servers button), which takes a moment after launch.
ui_activate() {
  tmo 15 osascript -e 'tell application id "com.tsout.conduit" to activate' >/dev/null 2>&1 || true
  sleep 0.5
  ui_wait "Servers" 30 || true
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

# ui_fill <field-label> <text> [secure]: focus a text field (by label or
# placeholder; AX keeps the placeholder attribute after text is entered),
# clear it and type <text>, then read the value back. Tries real key events
# first (what a user does), then System Events keystrokes, then setting the
# accessibility value, and records which input method worked. A secure field
# only has to become non-empty, since its value reads back masked.
UI_INPUT_METHODS="$EVIDENCE/logs/ui-input-methods.txt"
ui_fill() {
  local field="$1" text="$2" secure="${3:-}" pid method got
  pid="$(app_pid)"
  [[ -n "$pid" ]] || { ui_evidence "fill $field"; return 1; }
  local methods="cgevent systemevents axvalue"
  # A secure field reads back as one bullet per character, so only the length
  # can be checked; skip methods that have shown they drop text.
  [[ -n "$secure" ]] && methods="systemevents cgevent axvalue"
  for method in $methods; do
    TPUI_TIMEOUT=30 tpui focus "$pid" "$field" >/dev/null 2>&1 || break
    sleep 0.3
    tpui key 0 cmd >/dev/null 2>&1 # select all
    tpui key 51 >/dev/null 2>&1    # delete
    case "$method" in
    cgevent) tpui keys "$text" >/dev/null 2>&1 ;;
    systemevents)
      tmo 30 osascript -e 'on run argv' -e 'tell application "System Events" to keystroke (item 1 of argv)' -e 'end run' "$text" >/dev/null 2>&1
      ;;
    axvalue) TPUI_TIMEOUT=30 tpui setvalue "$pid" "$field" "$text" >/dev/null 2>&1 ;;
    esac
    sleep 0.5
    got="$(TPUI_TIMEOUT=20 tpui value "$pid" "$field" 2>/dev/null)"
    if [[ "$got" == "$text" ]] || [[ -n "$secure" && ${#got} -eq ${#text} ]]; then
      printf '%s\t%s\n' "$field" "$method" >>"$UI_INPUT_METHODS"
      printf '[ui] filled "%s" with %s\n' "$field" "$method" >&2
      return 0
    fi
    printf '[ui] %s did not fill "%s" (length now %s)\n' "$method" "$field" "${#got}" >&2
  done
  ui_evidence "fill $field"
  return 1
}

# ui_gone <text> [seconds]: wait until no element labelled <text> exists.
# Only tpui's "not found" exit (2) counts as gone; a probe error never does.
ui_gone() {
  local text="$1" secs="${2:-15}" i pid rc
  for ((i = 0; i < secs; i++)); do
    pid="$(app_pid)"
    [[ -n "$pid" ]] || return 2
    TPUI_TIMEOUT=20 tpui find "$pid" "$text" >/dev/null 2>&1
    rc=$?
    [[ $rc -eq 2 ]] && return 0
    sleep 1
  done
  return 1
}

# no_prompt_check <label>: record whether a keychain or other system prompt is
# on screen now. Any such window would block a real user and hang an
# unattended run. A failed or unparseable probe is a FAIL, never a PASS.
no_prompt_check() {
  local what rc
  what="$(winq prompt 2>&1)"
  rc=$?
  case $rc in
  0)
    shot "prompt-$(_slug "$1")" >/dev/null || true
    fail "no keychain or system prompt ($1)" "$what (screenshots/prompt-$(_slug "$1").png)"
    return 1
    ;;
  1) pass "no keychain or system prompt ($1)" ;;
  *)
    fail "no keychain or system prompt ($1)" "could not read the window list (probe exit $rc): $what"
    return 1
    ;;
  esac
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

# selftest_check <name> <log>: the gateway reads the server's token back from
# the keychain. Needs SERVER_ID. A hang or a missing OK line is a FAIL.
selftest_check() { # selftest_check <name> <log>
  local name="$1" log="$2" rc
  tmo 60 "$GATEWAY" --selftest-secrets >"$log" 2>&1
  rc=$?
  if [[ $rc -eq 0 ]] && grep -Eq "^OK +$SERVER_ID :: MOCK_API_TOKEN" "$log"; then
    pass "$name"
  elif [[ $rc -eq 142 ]]; then
    fail "$name" "gateway --selftest-secrets hung for 60s (a keychain prompt would do this)"
  else
    fail "$name" "exit $rc: $(tr '\n' ' ' <"$log" | cut -c1-300)"
  fi
}

# tool_call <entry.json> <tool> <out.json>: make a real MCP call through a
# client entry, pressing Approve if Toolport holds it. Sets TOOL_CALL_DETAIL.
# Returns 0 only when the call succeeded and, for check_secret, the mock
# server reported "token matches". Unparseable output is a failure.
tool_call() {
  local entry="$1" tool="$2" out="$3" call rc approved summary
  MCP_CALL_TIMEOUT_MS=90000 node "$_UI_HERE/mcp-call.mjs" "$entry" "$tool" >"$out" 2>&1 &
  call=$!
  approved="$(approve_pending 95 "$call")"
  wait "$call"
  rc=$?
  summary="$(python3 -c '
import json, sys
try:
    d = json.load(open(sys.argv[1]))
except Exception as e:
    print("unparseable call report: %s" % e)
    sys.exit(1)
ok = d.get("ok") is True and (sys.argv[2] != "check_secret" or d.get("result") == "token matches")
print("%s | via %s | %s" % (str(d.get("result", ""))[:120], d.get("calledVia", ""), d.get("detail", "")))
sys.exit(0 if ok else 1)' "$out" "$tool")"
  local parsed=$?
  TOOL_CALL_DETAIL="exit $rc: $summary; approvals=$approved"
  [[ $rc -eq 0 && $parsed -eq 0 ]]
}

# set_launch_at_login <on|off>: flip the Settings > General switch until the
# LaunchAgent on disk matches. Returns 0 when it does; the plist is the truth,
# not the switch's look. Leaves Settings > General open.
set_launch_at_login() {
  local want="$1" n state
  ui_press "Settings" >/dev/null 2>&1 && ui_press "General" >/dev/null 2>&1 || return 1
  for n in 1 -1 2; do
    state="$(login_item_state)"
    [[ "$want" == on && "$state" == ok* ]] && return 0
    [[ "$want" == off && "$state" == absent ]] && return 0
    TPUI_TIMEOUT=30 tpui press "$(app_pid)" "Launch at login" "$n" >&2 2>/dev/null || continue
    if [[ "$want" == on ]]; then wait_for 10 test -e "$LOGIN_PLIST"; else wait_for 10 test ! -e "$LOGIN_PLIST"; fi
  done
  state="$(login_item_state)"
  [[ "$want" == on && "$state" == ok* ]] || [[ "$want" == off && "$state" == absent ]]
}

# quit_app: 0 quit normally, 1 had to be killed, 3 was not running.
quit_app() {
  local pid
  pid="$(app_pid)"
  [[ -z "$pid" ]] && return 3
  tmo 20 osascript -e 'tell application id "com.tsout.conduit" to quit' >/dev/null 2>&1 || true
  if ! wait_for 20 bash -c "! ps -p $pid >/dev/null"; then
    kill "$pid" 2>/dev/null || true
    wait_for 10 bash -c "! ps -p $pid >/dev/null" || kill -9 "$pid" 2>/dev/null || true
    return 1
  fi
}

# quit_check <check-name>: quit the app and record the outcome.
quit_check() {
  quit_app
  case $? in
  0) pass "$1" ;;
  3) fail "$1" "the app was not running" ;;
  *) fail "$1" "did not quit within 20s and had to be killed" ;;
  esac
}
