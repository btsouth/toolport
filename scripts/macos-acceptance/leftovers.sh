#!/usr/bin/env bash
# List everything Toolport left on the machine after an uninstall: files,
# LaunchAgents, login items, defaults domains, keychain items and processes.
#
# usage: leftovers.sh <label> [--expect-clean] [--login-item-left-on]
#   --expect-clean: after "Remove Toolport data", anything beyond the files
#   the docs say are kept is a FAIL; otherwise leftovers are listed as notes.
#   --login-item-left-on: the uninstall skipped turning off Launch at login on
#   purpose; report the login item it leaves instead of failing on it.
set -uo pipefail
LABEL="$1"
CLEAN=0
LEFT_ON=0
for a in "${@:2}"; do
  [[ "$a" == "--expect-clean" ]] && CLEAN=1
  [[ "$a" == "--login-item-left-on" ]] && LEFT_ON=1
done
AREA="leftovers ($LABEL)"
HERE="$(cd "$(dirname "$0")" && pwd)"
# shellcheck source=common.sh
source "$HERE/common.sh"
OUT="$EVIDENCE/leftovers-$LABEL.txt"
PATTERN='toolport|conduit|com\.tsout'

# Each probe writes its own section file and its exit status, so a probe that
# failed or timed out is reported as such instead of reading as "none left".
SEC="$EVIDENCE/logs/leftovers-$(printf '%s' "$LABEL" | tr -c 'A-Za-z0-9._-' '_')"
mkdir -p "$SEC"
probe() { # probe <name> <command...>: run, keep output and exit status
  local name="$1"
  shift
  "$@" >"$SEC/$name.txt" 2>"$SEC/$name.err"
  echo $? >"$SEC/$name.rc"
}
rc_of() { cat "$SEC/$1.rc"; }
hits() { grep -iE "$PATTERN" "$SEC/$1.txt" | grep -v -e 'grep ' -e leftovers.sh; }

probe ps ps -axo pid,ppid,command
probe find tmo 300 find "$HOME" \( -path "$HOME/work" -o -path "$HOME/hostedtoolcache" -o -path "$HOME/Library/Keychains" \
  -o -path "$HOME/mcp-servers" -o -path "$HOME/.Trash" -o -path "$HOME/Downloads" -o -path "$HOME/Library/Caches/Homebrew" \) -prune -o \
  \( -iname '*toolport*' -o -iname '*conduit*' -o -iname '*com.tsout*' \) -print
probe data bash -c 'for d in "$HOME/Library/Application Support/Toolport" "$HOME/Library/Application Support/Conduit"; do [[ -d "$d" ]] || continue; find "$d" -mindepth 1 -print || exit 1; done'
probe agents ls -1 "$HOME/Library/LaunchAgents"
probe launchctl tmo 20 launchctl list
probe loginitems tmo 20 osascript -e 'tell application "System Events" to get the name of every login item'
probe domains tmo 30 defaults domains
probe keychain tmo 30 security dump-keychain "$HOME/Library/Keychains/login.keychain-db"
# sfltool dumpbtm asks for an administrator password, so it is not used.

{
  echo "# Toolport leftovers after: $LABEL"
  echo
  echo "## Processes"; hits ps || echo "(none)"
  echo; echo "## Applications"; ls -d /Applications/Toolport.app "$HOME/Applications/Toolport.app" 2>/dev/null || echo "(none)"
  echo; echo "## Files and folders under ~ (excluding the runner's work folders and the test server)"; sort "$SEC/find.txt"
  echo; echo "## Toolport data folder contents"; sort "$SEC/data.txt"
  echo; echo "## LaunchAgents"; hits agents || echo "(none in ~/Library/LaunchAgents)"; hits launchctl || echo "(none loaded)"
  echo; echo "## Login items (System Events)"; cat "$SEC/loginitems.txt" "$SEC/loginitems.err"
  echo; echo "## defaults domains"; tr ',' '\n' <"$SEC/domains.txt" | sed 's/^ //' | grep -iE "$PATTERN" || echo "(none)"
  echo; echo "## Login keychain items (attributes only)"
  awk '/^keychain:/{if (blk ~ /conduit|toolport|tsout/) print blk; blk=$0; next} {blk=blk "\n" $0} END {if (blk ~ /conduit|toolport|tsout/) print blk}' "$SEC/keychain.txt"
} >"$OUT" 2>&1

count() { grep -c . <<<"$1"; }
info "note: full list" "leftovers-$LABEL.txt"

if [[ "$(rc_of ps)" -ne 0 || ! -s "$SEC/ps.txt" ]]; then
  fail "no Toolport process left running" "ps failed (exit $(rc_of ps))"
elif procs="$(hits ps)"; then
  fail "no Toolport process left running" "$(count "$procs") process(es): $(cut -c1-200 <<<"$procs" | tr '\n' ';')"
else
  pass "no Toolport process left running"
fi

agent_hits="$(hits agents; hits launchctl)"
agents_rc="$(rc_of agents)"
if [[ "$(rc_of launchctl)" -ne 0 ]]; then
  fail "no LaunchAgent left" "launchctl list failed (exit $(rc_of launchctl))"
elif [[ "$agents_rc" -ne 0 && -d "$HOME/Library/LaunchAgents" ]]; then
  fail "no LaunchAgent left" "could not list ~/Library/LaunchAgents (exit $agents_rc)"
elif [[ -n "$agent_hits" && $LEFT_ON -eq 1 ]]; then
  info "note: login item left by an uninstall with Launch at login still on" "$(login_item_state); $(tr '\n' ' ' <<<"$agent_hits" | cut -c1-200). launchd tries it at every login and fails because the app is gone"
elif [[ $LEFT_ON -eq 1 ]]; then
  info "note: login item left by an uninstall with Launch at login still on" "none (the plist was already gone)"
elif [[ -n "$agent_hits" ]]; then
  fail "no LaunchAgent left" "$(tr '\n' ' ' <<<"$agent_hits" | cut -c1-300) (a login item whose app is gone)"
else
  pass "no LaunchAgent left" "none in ~/Library/LaunchAgents and none loaded"
fi

if [[ "$(rc_of loginitems)" -ne 0 ]]; then
  untested "no Toolport login item left (System Events)" "osascript exit $(rc_of loginitems): $(head -c 200 "$SEC/loginitems.err")"
elif grep -qiE "$PATTERN" "$SEC/loginitems.txt"; then
  fail "no Toolport login item left (System Events)" "$(cat "$SEC/loginitems.txt")"
else
  pass "no Toolport login item left (System Events)" "$(head -c 200 "$SEC/loginitems.txt")"
fi

find_rc="$(rc_of find)"
files="$(sort "$SEC/find.txt")"
if [[ "$find_rc" -eq 142 ]]; then
  fail "home folder scanned for leftovers" "find hit its 300s limit; the list is incomplete"
elif [[ "$find_rc" -ne 0 ]]; then
  # find exits 1 when some folders are unreadable (macOS privacy controls); the
  # rest is still scanned, so name what could not be read.
  untested "leftovers in folders the runner cannot read" "find exit $find_rc: $(grep -c . "$SEC/find.err") unreadable paths (logs/leftovers-*/find.err)"
fi
info "note: files and folders left under ~" "$(count "$files")"

data_rc="$(rc_of data)"
data_files="$(cat "$SEC/data.txt")"
[[ "$data_rc" -eq 0 ]] || fail "Toolport data folder listed" "exit $data_rc: $(head -c 200 "$SEC/data.err")"
info "note: data folder entries left" "$(count "$data_files")"

if [[ "$(rc_of domains)" -eq 0 ]]; then
  info "note: defaults domains left" "$(tr ',' '\n' <"$SEC/domains.txt" | grep -ciE "$PATTERN")"
else
  untested "defaults domains left" "defaults domains failed (exit $(rc_of domains))"
fi
if [[ "$(rc_of keychain)" -eq 0 ]]; then
  info "note: login keychain items left" "$(grep -c '"svce"' <(sed -n '/^## Login keychain/,$p' "$OUT"))"
else
  untested "login keychain items left" "security dump-keychain failed (exit $(rc_of keychain))"
fi
untested "data-protection keychain items left" "only a binary signed with Toolport's team access group can list them; the app was removed"

if [[ $CLEAN -eq 1 ]]; then
  # After Remove Toolport data on a clean runner, the docs promise only the
  # report file and empty folders remain (the runner had no unrelated files in
  # Toolport's folders). Any file inside the data folder is a leftover too.
  extra="$( { grep '^/' <<<"$files"; grep '^/' <<<"$data_files"; } |
    grep -vi -e 'toolport-removal-report' | while IFS= read -r p; do
      [[ -d "$p" && -z "$(ls -A "$p" 2>/dev/null)" ]] || printf '%s\n' "$p"
    done | grep -v -x -e "$HOME/Library/Application Support/Toolport" -e "$HOME/Library/Application Support/Conduit" | sort -u | head -20)"
  if [[ "$find_rc" -eq 142 || "$data_rc" -ne 0 ]]; then
    fail "only the removal report and empty folders remain" "the scan was incomplete"
  elif [[ -z "$extra" ]]; then
    pass "only the removal report and empty folders remain"
  else
    fail "only the removal report and empty folders remain" "$(tr '\n' ' ' <<<"$extra" | cut -c1-400)"
  fi
fi
finish_step
