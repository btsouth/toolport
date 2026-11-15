#!/usr/bin/env bash
# List everything Toolport left on the machine after an uninstall: files,
# LaunchAgents, login items, defaults domains, keychain items and processes.
#
# usage: leftovers.sh <label> [--expect-clean]
#   --expect-clean: after "Remove Toolport data", anything beyond the files
#   the docs say are kept is a FAIL; otherwise leftovers are listed as notes.
set -uo pipefail
LABEL="$1"
CLEAN=0
[[ "${2:-}" == "--expect-clean" ]] && CLEAN=1
AREA="leftovers ($LABEL)"
HERE="$(cd "$(dirname "$0")" && pwd)"
# shellcheck source=common.sh
source "$HERE/common.sh"
OUT="$EVIDENCE/leftovers-$LABEL.txt"
PATTERN='toolport|conduit|com\.tsout'

{
  echo "# Toolport leftovers after: $LABEL"
  echo
  echo "## Processes"
  ps -axo pid,ppid,command | grep -iE "$PATTERN" | grep -v -e grep -e leftovers.sh || echo "(none)"
  echo
  echo "## Applications"
  ls -d /Applications/Toolport.app "$HOME/Applications/Toolport.app" 2>/dev/null || echo "(none)"
  echo
  echo "## Files and folders under ~ (excluding the runner's work folders and the test server)"
  tmo 300 find "$HOME" \( -path "$HOME/work" -o -path "$HOME/hostedtoolcache" -o -path "$HOME/Library/Keychains" \
    -o -path "$HOME/mcp-servers" -o -path "$HOME/.Trash" -o -path "$HOME/Downloads" -o -path "$HOME/Library/Caches/Homebrew" \) -prune -o \
    \( -iname '*toolport*' -o -iname '*conduit*' -o -iname '*com.tsout*' \) -print 2>/dev/null | sort || true
  echo
  echo "## Toolport data folder contents"
  for d in "$HOME/Library/Application Support/Toolport" "$HOME/Library/Application Support/Conduit"; do
    [[ -d "$d" ]] && find "$d" -print | sort
  done
  echo
  echo "## LaunchAgents"
  ls -la "$HOME/Library/LaunchAgents" 2>/dev/null | grep -iE "$PATTERN" || echo "(none in ~/Library/LaunchAgents)"
  tmo 20 launchctl list 2>/dev/null | grep -iE "$PATTERN" || echo "(none loaded)"
  echo
  echo "## Login items (System Events)"
  tmo 20 osascript -e 'tell application "System Events" to get the name of every login item' </dev/null 2>&1 || true
  # sfltool dumpbtm asks for an administrator password, so it is not used.
  echo
  echo "## defaults domains"
  tmo 30 defaults domains 2>/dev/null | tr ',' '\n' | sed 's/^ //' | grep -iE "$PATTERN" || echo "(none)"
  echo
  echo "## Login keychain items (attributes only)"
  tmo 30 security dump-keychain "$HOME/Library/Keychains/login.keychain-db" 2>/dev/null |
    awk '/^keychain:/{if (blk ~ /conduit|toolport|tsout/) print blk; blk=$0; next} {blk=blk "\n" $0} END {if (blk ~ /conduit|toolport|tsout/) print blk}' ||
    true
} >"$OUT" 2>&1

count_section() {
  awk -v s="$1" '$0 == "## " s {on=1; next} /^## /{on=0} on && NF && $0 !~ /^\(/ {n++} END {print n+0}' "$OUT"
}
procs="$(count_section Processes)"
files="$(count_section 'Files and folders under ~ (excluding the runner'"'"'s work folders and the test server)')"
data="$(count_section 'Toolport data folder contents')"
agents="$(grep -cE '\.plist' <(sed -n '/^## LaunchAgents/,/^## Login items/p' "$OUT") || true)"
domains="$(count_section 'defaults domains')"
keys="$(grep -c '"svce"' <(sed -n '/^## Login keychain/,$p' "$OUT") || true)"

info "full list" "leftovers-$LABEL.txt"
if [[ "$procs" -eq 0 ]]; then pass "no Toolport process left running"; else fail "no Toolport process left running" "$procs processes (see leftovers-$LABEL.txt)"; fi
if [[ "$agents" -eq 0 ]]; then pass "no LaunchAgent left"; else fail "no LaunchAgent left" "$agents plist(s) (see leftovers-$LABEL.txt)"; fi
info "files and folders left under ~" "$files"
info "data folder entries left" "$data"
info "defaults domains left" "$domains"
info "login keychain items left" "$keys"
untested "data-protection keychain items left" "only a binary signed with Toolport's team access group can list them; the app was removed"
if [[ $CLEAN -eq 1 ]]; then
  # After Remove Toolport data the docs promise only the report file and
  # folders that held unrelated files remain.
  extra="$(sed -n '/^## Files and folders/,/^## Toolport data folder/p' "$OUT" | grep '^/' |
    grep -vi -e 'toolport-removal-report' -e '/Toolport$' -e '/Conduit$' | head -20)"
  if [[ -z "$extra" ]]; then
    pass "only the removal report and empty folders remain"
  else
    fail "only the removal report and empty folders remain" "$(printf '%s ' $extra | cut -c1-400)"
  fi
fi
finish_step
