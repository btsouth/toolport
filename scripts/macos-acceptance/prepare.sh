#!/usr/bin/env bash
# Prepare a clean runner for an acceptance leg: evidence folders, the window
# and accessibility probe, a throwaway API token, and realistic client configs
# seeded before Toolport is first opened (a new Mac user already has their AI
# clients installed).
#
# usage: prepare.sh   (in GitHub Actions; writes EVIDENCE, TPUI and SECRET_FILE to $GITHUB_ENV)
set -euo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
EVIDENCE="$RUNNER_TEMP/macos-acceptance"
TPUI="$EVIDENCE/bin/tpui"
SECRET_FILE="$RUNNER_TEMP/acceptance-secret"
mkdir -p "$EVIDENCE/logs" "$EVIDENCE/bin" "$EVIDENCE/screenshots"
{
  echo "EVIDENCE=$EVIDENCE"
  echo "TPUI=$TPUI"
  echo "SECRET_FILE=$SECRET_FILE"
  echo "CLIENT_MANIFEST=$EVIDENCE/client-manifest.json"
} >>"$GITHUB_ENV"

{
  sw_vers
  uname -a
  id
  echo "console user: $(stat -f %Su /dev/console)"
  echo "launchd manager: $(launchctl managername)"
  system_profiler SPDisplaysDataType 2>/dev/null | grep -E 'Resolution|Display Type' || true
  echo "node: $(command -v node) $(node --version)"
  echo "python: $(python3 --version)"
} | tee "$EVIDENCE/logs/runner.txt"
printf 'runner\tINFO\tmacOS\t%s\n' "$(sw_vers -productVersion) $(uname -m), console user $(stat -f %Su /dev/console)" >>"$EVIDENCE/results.tsv"

swiftc -O "$HERE/tpui.swift" -o "$TPUI"

umask 077
printf 'tp-acceptance-%s' "$(uuidgen)" >"$SECRET_FILE"

python3 -m pip install --quiet --user --break-system-packages pyyaml >/dev/null 2>&1 || true
python3 "$HERE/clients.py" seed "$HOME" "$EVIDENCE/client-manifest.json" >>"$EVIDENCE/results.tsv"
