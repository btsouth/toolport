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

# shot <name>: full-screen capture without the shutter sound.
shot() {
  local file="$EVIDENCE/screenshots/$1.png"
  if screencapture -x "$file" 2>"$EVIDENCE/logs/screencapture-$1.err"; then
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

# The tool the UI and window probes use. Built by the workflow from tpui.swift.
TPUI="${TPUI:-$EVIDENCE/bin/tpui}"
APP_PATH="${APP_PATH:-/Applications/Toolport.app}"
GATEWAY="$APP_PATH/Contents/MacOS/toolport-gateway"
DATA_DIR="$HOME/Library/Application Support/Toolport"
