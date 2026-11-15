# shellcheck shell=bash
# Shared helpers for the macOS acceptance job. Source this file; do not run it.
#
# Every check records one row in "$EVIDENCE/results.tsv":
#   <area>\t<PASS|FAIL|UNTESTED|INFO>\t<check>\t<detail>
# render-summary.py turns that file into the job summary. A step calls
# finish_step at the end, which exits non-zero when the step recorded a FAIL,
# so a broken check fails the job instead of only appearing in the table.

set -uo pipefail

: "${EVIDENCE:=${RUNNER_TEMP:-/tmp}/macos-acceptance}"
mkdir -p "$EVIDENCE/logs" "$EVIDENCE/screenshots"
RESULTS="$EVIDENCE/results.tsv"
touch "$RESULTS"
STEP_FAILED=0
AREA="${AREA:-general}"

log() { printf '\033[1m== %s ==\033[0m\n' "$*" >&2; }

_record() {
  local status="$1" check="$2" detail="${3:-}"
  detail="${detail//$'\t'/ }"
  detail="${detail//$'\n'/ }"
  printf '%s\t%s\t%s\t%s\n' "$AREA" "$status" "$check" "$detail" >>"$RESULTS"
}

pass() {
  _record PASS "$1" "${2:-}"
  printf '[PASS] %s %s\n' "$1" "${2:-}" >&2
}

fail() {
  _record FAIL "$1" "${2:-}"
  STEP_FAILED=1
  printf '::error title=%s::%s\n' "$1" "${2:-$1}" >&2
}

untested() {
  _record UNTESTED "$1" "${2:-}"
  printf '::warning title=Untested: %s::%s\n' "$1" "${2:-}" >&2
}

info() {
  _record INFO "$1" "${2:-}"
  printf '[INFO] %s %s\n' "$1" "${2:-}" >&2
}

# check <name> <command...>: run the command, save its output under logs/, and
# record PASS or FAIL from its exit status.
check() {
  local name="$1"
  shift
  local slug out rc
  slug="$(printf '%s' "$AREA-$name" | tr -c 'A-Za-z0-9._-' '_' | cut -c1-80)"
  out="$EVIDENCE/logs/$slug.log"
  "$@" >"$out" 2>&1
  rc=$?
  if [[ $rc -eq 0 ]]; then
    pass "$name" "$(tail -n 2 "$out" | tr '\n' ' ' | cut -c1-200)"
  else
    fail "$name" "exit $rc: $(tail -n 3 "$out" | tr '\n' ' ' | cut -c1-300) (log: logs/$slug.log)"
  fi
  return $rc
}

finish_step() {
  if [[ $STEP_FAILED -ne 0 ]]; then
    printf '::error::%s checks failed; see the job summary and logs.\n' "$AREA" >&2
    exit 1
  fi
}

# tmo <seconds> <command...>: run with a hard wall-clock limit (macOS has no
# GNU timeout). Exit 142 means the limit was hit.
tmo() {
  local secs="$1"
  shift
  perl -e 'alarm shift; exec @ARGV' "$secs" "$@"
}

# shot <name>: full-screen capture without the shutter sound.
shot() {
  local file="$EVIDENCE/screenshots/$1.png"
  if tmo 20 screencapture -x "$file" 2>"$EVIDENCE/logs/screencapture-$1.err"; then
    printf '%s\n' "$file"
  else
    return 1
  fi
}

# wait_for <seconds> <command...>: poll once a second until the command succeeds.
wait_for() {
  local secs="$1"
  shift
  local i
  for ((i = 0; i < secs; i++)); do
    "$@" >/dev/null 2>&1 && return 0
    sleep 1
  done
  return 1
}

# pids_of <fixed-string>: pids whose command line contains the string. ps is
# used instead of pgrep -f, which missed the app on the runner.
pids_of() { ps -axo pid=,command= | grep -F -- "$1" | grep -v -e 'grep -F' | awk '{print $1}'; }

# daemon_gone_check <check-name> <seconds>: no toolport-gateway daemon may be
# left once nothing is connected. Toolport stops it on quit, on Remove from all
# clients and when its binary is removed; waiting out the idle grace is a bug.
daemon_gone_check() {
  local name="$1" secs="$2"
  if wait_for "$secs" bash -c "! ps -axo command= | grep -F 'toolport-gateway --daemon' | grep -vq grep"; then
    pass "$name" "none within ${secs}s"
  else
    fail "$name" "still running after ${secs}s: $(ps -axo pid=,ppid=,command= | grep -F 'toolport-gateway --daemon' | grep -v grep | tr '\n' ' ' | cut -c1-300)"
  fi
}

# The tool the UI and window probes use. Built by the workflow from tpui.swift.
TPUI_BIN="${TPUI:-$EVIDENCE/bin/tpui}"
# Every probe call is bounded: an unresponsive app makes each AX call wait.
tpui() { tmo "${TPUI_TIMEOUT:-60}" "$TPUI_BIN" "$@"; }
# winq <query> [arg]: ask windows_query.py about the current window list.
# 0 found, 1 none, 2 the list was unusable, 3 the probe itself failed.
winq() {
  local json
  json="$(TPUI_TIMEOUT=15 tpui windows 2>/dev/null)" || return 3
  printf '%s' "$json" | python3 "$(dirname "${BASH_SOURCE[0]}")/windows_query.py" "$@"
}

APP_PATH="${APP_PATH:-/Applications/Toolport.app}"
GATEWAY="$APP_PATH/Contents/MacOS/toolport-gateway"
DATA_DIR="$HOME/Library/Application Support/Toolport"

# record_rows <area> <name> <command...>: append the result rows a helper such
# as clients.py prints, under <area>. The helper exits 0 (all passed) or 1 (a
# row failed); any other exit, or no rows at all, is a FAIL of its own, so a
# crashed helper can never look like a clean result.
record_rows() {
  local area="$1" name="$2" out rc err
  shift 2
  err="$EVIDENCE/logs/$(printf '%s' "$area-$name" | tr -c 'A-Za-z0-9._-' '_' | cut -c1-80).err"
  out="$("$@" 2>"$err")"
  rc=$?
  if [[ $rc -gt 1 || -z "$out" ]] || ! grep -qE $'\t(PASS|FAIL|UNTESTED|INFO)\t' <<<"$out"; then
    fail "$name" "helper exited $rc with no usable rows: $(tail -c 300 "$err" | tr '\n' ' ')"
    return 1
  fi
  printf '%s\n' "$out" | awk -F'\t' -v OFS='\t' -v a="$area" '{ $1 = a; print }' >>"$RESULTS"
  if grep -q $'\tFAIL\t' <<<"$out"; then
    STEP_FAILED=1
    return 1
  fi
  [[ $rc -eq 0 ]] || { fail "$name" "helper exited 1 without a FAIL row"; return 1; }
}

# client_list <manifest>: "id<TAB>name" lines, or exit non-zero (never an empty
# success) so loops over clients cannot silently check nothing.
client_list() {
  local out
  out="$(python3 -c 'import json,sys; [print(k + "\t" + v["name"]) for k, v in json.load(open(sys.argv[1])).items()]' "$1")" || return 1
  [[ -n "$out" ]] || return 1
  printf '%s\n' "$out"
}

# Launch at login: tauri-plugin-autostart writes this LaunchAgent (not an
# SMAppService registration). launchd loads it at the next login.
LOGIN_PLIST="$HOME/Library/LaunchAgents/Toolport.plist"
# login_item_state: prints "absent", "ok <program>" or "bad <why>"; returns 0
# for absent/ok, 1 for a plist that would not start Toolport, 2 if unreadable.
login_item_state() {
  [[ -e "$LOGIN_PLIST" ]] || { echo absent; return 0; }
  local json
  json="$(plutil -convert json -o - "$LOGIN_PLIST" 2>&1)" || { echo "bad unreadable plist: $json"; return 2; }
  python3 -c '
import json, os, sys
d = json.loads(sys.argv[1])
args = d.get("ProgramArguments") or []
prog = args[0] if args else d.get("Program", "")
why = []
if d.get("Label") != "Toolport": why.append("Label is %r" % d.get("Label"))
if not prog: why.append("no program")
elif not os.access(prog, os.X_OK): why.append("%s does not exist or is not executable" % prog)
if "--hidden" not in args: why.append("no --hidden argument")
if d.get("RunAtLoad") is not True: why.append("RunAtLoad is not true")
print(("bad " + "; ".join(why) + " (program " + str(prog) + ")") if why else ("ok " + prog))
sys.exit(1 if why else 0)' "$json"
}
