#!/usr/bin/env bash
# Collect crash reports and Toolport's own logs into the evidence folder, then
# write the job summary. Exits 1 when any check failed, so the job ends red
# with the reason in the summary.
#
# usage: finish.sh <title> [job-status]
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
: "${EVIDENCE:=${RUNNER_TEMP:-/tmp}/macos-acceptance}"
mkdir -p "$EVIDENCE/crash-reports" "$EVIDENCE/toolport-logs"
for dir in "$HOME/Library/Logs/DiagnosticReports" /Library/Logs/DiagnosticReports; do
  find "$dir" -maxdepth 2 -type f 2>/dev/null | grep -iE 'toolport|conduit' | while read -r f; do
    cp "$f" "$EVIDENCE/crash-reports/" 2>/dev/null || true
  done
done
for dir in "$HOME/Library/Application Support/Toolport" "$HOME/Library/Logs/com.tsout.conduit" "$HOME/.Trash"; do
  [[ -d "$dir" ]] || continue
  find "$dir" -maxdepth 3 -type f \( -name '*.log' -o -name '*.jsonl' -o -name '*report*' \) -size -20M 2>/dev/null |
    while read -r f; do cp "$f" "$EVIDENCE/toolport-logs/" 2>/dev/null || true; done
done
find "$HOME" -maxdepth 1 -iname '*toolport*report*' -exec cp {} "$EVIDENCE/toolport-logs/" \; 2>/dev/null || true
if [[ -n "$(ls -A "$EVIDENCE/crash-reports")" ]]; then
  printf 'crash reports\tFAIL\tno Toolport crash reports\t%s\n' "$(ls "$EVIDENCE/crash-reports" | tr '\n' ' ')" >>"$EVIDENCE/results.tsv"
else
  printf 'crash reports\tPASS\tno Toolport crash reports\t%s\n' "searched ~/Library/Logs/DiagnosticReports and /Library/Logs/DiagnosticReports" >>"$EVIDENCE/results.tsv"
fi
# The throwaway token must never reach the uploaded evidence.
if [[ -n "${SECRET_FILE:-}" && -s "$SECRET_FILE" ]]; then
  grep -rIlF -- "$(cat "$SECRET_FILE")" "$EVIDENCE" >"$EVIDENCE/logs/secret-in-evidence.txt" 2>"$EVIDENCE/logs/secret-in-evidence.err"
  rc=$?
  if [[ $rc -eq 0 ]]; then
    printf 'evidence\tFAIL\ttest token kept out of logs\t%s\n' "$(tr '\n' ' ' <"$EVIDENCE/logs/secret-in-evidence.txt")" >>"$EVIDENCE/results.tsv"
    while read -r f; do rm -f "$f"; done <"$EVIDENCE/logs/secret-in-evidence.txt"
  elif [[ $rc -eq 1 ]]; then
    printf 'evidence\tPASS\ttest token kept out of logs\t\n' >>"$EVIDENCE/results.tsv"
  else
    printf 'evidence\tFAIL\ttest token kept out of logs\tthe search failed (grep exit %s); evidence removed\n' "$rc" >>"$EVIDENCE/results.tsv"
    find "$EVIDENCE" -mindepth 1 -maxdepth 1 ! -name results.tsv -exec rm -rf {} +
  fi
fi
python3 "$HERE/render_summary.py" "$EVIDENCE/results.tsv" "$1" "${2:-}" | tee "$EVIDENCE/summary.md" >>"${GITHUB_STEP_SUMMARY:-/dev/null}"
exit "${PIPESTATUS[0]}"
