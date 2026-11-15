#!/usr/bin/env bash
# The Homebrew leg: install the cask from the tap the way the docs say, check
# what it installed, then uninstall with zap and compare with docs/uninstall.md.
#
# usage: homebrew.sh install <expected-tag>
#        homebrew.sh uninstall <client-manifest.json>
# The tap's cask tracks only the latest release; the workflow skips this leg
# (with an UNTESTED row) when an older tag is under test.
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
AREA="homebrew"
# shellcheck source=common.sh
source "$HERE/common.sh"
TAP_CASK="btsouth/toolport/toolport"
export HOMEBREW_NO_AUTO_UPDATE=1 HOMEBREW_NO_INSTALL_CLEANUP=1 HOMEBREW_NO_ANALYTICS=1

case "$1" in
install)
  want="${2#v}"
  brew tap btsouth/toolport >"$EVIDENCE/logs/brew-tap.log" 2>&1 || { fail "brew tap btsouth/toolport" "$(tail -n 3 "$EVIDENCE/logs/brew-tap.log")"; finish_step; }
  cask_file="$(brew --repository btsouth/toolport)/Casks/toolport.rb"
  if ! [[ -s "$cask_file" ]] || ! cp "$cask_file" "$EVIDENCE/logs/live-cask.rb"; then
    fail "tap cask readable" "$cask_file is missing or empty"
    finish_step
  fi
  tap_version="$(sed -n 's/^ *version "\(.*\)"/\1/p' "$cask_file" | head -1)"
  if [[ "$tap_version" == "$want" ]]; then
    pass "tap cask tracks the release" "$tap_version"
  else
    fail "tap cask tracks the release" "tap has $tap_version, release under test is $want"
  fi
  # The repo's snapshot of the cask is what docs/uninstall.md describes.
  if diff -u "$GITHUB_WORKSPACE/packaging/homebrew/toolport.rb" "$cask_file" >"$EVIDENCE/logs/cask-vs-repo.diff" 2>&1; then
    pass "live cask matches packaging/homebrew/toolport.rb"
  else
    fail "live cask matches packaging/homebrew/toolport.rb" "differs (logs/cask-vs-repo.diff): $(grep -E '^[-+] ' "$EVIDENCE/logs/cask-vs-repo.diff" | head -8 | tr '\n' ' ' | cut -c1-400)"
  fi
  # docs/uninstall.md: uninstall restores clients (never on upgrade or
  # reinstall), zap removes data through Toolport's own removal.
  if grep -q '"--disconnect-all", "--uninstall"' "$cask_file" && grep -q 'uninstall_existing_cask' "$cask_file"; then
    pass "cask restores clients on uninstall and skips upgrades and reinstalls"
  else
    fail "cask restores clients on uninstall and skips upgrades and reinstalls" "no upgrade-aware --disconnect-all --uninstall in uninstall_preflight"
  fi
  zap_data="$(sed -n '/zap trash:/,/rmdir:/p' "$cask_file" | grep -nE 'Application Support/(Toolport|Conduit)' | tr '\n' ' ')"
  if [[ -z "$zap_data" ]] && grep -q '"--remove-data", "--confirm"' "$cask_file"; then
    pass "cask zap removes data only through Toolport's own removal"
  else
    fail "cask zap removes data only through Toolport's own removal" "zap trashes the data folder or skips --remove-data: $zap_data"
  fi
  if check "brew install --cask $TAP_CASK" brew install --cask "$TAP_CASK"; then
    q="$(xattr -p com.apple.quarantine "$APP_PATH" 2>/dev/null)"
    info "note: installed app quarantine" "${q:-none}"
  fi
  brew info --cask toolport >"$EVIDENCE/logs/brew-info.log" 2>&1
  ;;
uninstall)
  MANIFEST="${2:-}"
  [[ -n "$MANIFEST" ]] || { fail "client manifest given" "no manifest"; finish_step; }
  SNAP="$EVIDENCE/logs/clients-before-brew-reinstall.json"
  python3 "$HERE/clients.py" snapshot "$HOME" "$MANIFEST" "$SNAP" >/dev/null 2>"$EVIDENCE/logs/snapshot-before-brew-reinstall.err" ||
    fail "client configs snapshot before brew reinstall" "$(tail -c 300 "$EVIDENCE/logs/snapshot-before-brew-reinstall.err")"
  # shellcheck source=ui.sh
  source "$HERE/ui.sh"
  quit_check "app quits before brew commands"
  # Reinstall runs the cask's uninstall steps too; clients must stay connected.
  check "brew reinstall --cask toolport" brew reinstall --cask toolport
  record_rows homebrew "client configs untouched by brew reinstall" python3 "$HERE/clients.py" unchanged "$HOME" "$SNAP"
  # Plain uninstall: the cask restores clients, removes launch at login, and
  # keeps every byte of data. No manual --disconnect-all first.
  had_registry=0
  [[ -f "$DATA_DIR/registry.json" ]] && had_registry=1
  check "brew uninstall --cask toolport" brew uninstall --cask toolport
  record_rows homebrew "client configs restored by brew uninstall" python3 "$HERE/clients.py" restored "$HOME" "$MANIFEST"
  if [[ $had_registry -eq 0 ]]; then
    untested "brew uninstall keeps registry.json" "no registry existed before uninstall"
  elif [[ -f "$DATA_DIR/registry.json" ]]; then
    pass "brew uninstall keeps registry.json"
  else
    fail "brew uninstall keeps registry.json" "$DATA_DIR/registry.json is gone"
  fi
  if [[ "$(login_item_state)" == absent ]]; then
    pass "brew uninstall leaves no launch-at-login entry"
  else
    fail "brew uninstall leaves no launch-at-login entry" "$(login_item_state)"
  fi
  daemon_gone_check "no gateway daemon after brew uninstall" 15
  if [[ -d "$APP_PATH" ]]; then fail "app removed" "$APP_PATH still exists"; else pass "app removed"; fi
  # Zap: reinstall, then zap removes data through Toolport's removal.
  check "brew install --cask $TAP_CASK (again, for zap)" brew install --cask "$TAP_CASK"
  find "$DATA_DIR" -maxdepth 2 -print >"$EVIDENCE/logs/data-dir-before-zap.txt" 2>&1
  check "brew uninstall --cask --zap toolport" brew uninstall --cask --zap toolport
  if [[ -f "$HOME/Toolport-removal-report.json" ]]; then
    cp "$HOME/Toolport-removal-report.json" "$EVIDENCE/logs/brew-zap-removal-report.json"
    left="$(python3 -c 'import json,sys; d=json.load(open(sys.argv[1])); print(len(d.get("leftovers") or []), json.dumps(d.get("leftovers"))[:300])' "$HOME/Toolport-removal-report.json" 2>&1)"
    if [[ "$left" == "0 "* ]]; then pass "brew zap data removal completes"; else fail "brew zap data removal completes" "$left"; fi
  else
    fail "brew zap data removal completes" "no ~/Toolport-removal-report.json"
  fi
  if [[ -f "$DATA_DIR/registry.json" ]]; then
    fail "brew zap removes registry.json" "still there (logs/data-dir-before-zap.txt lists what was there)"
  else
    pass "brew zap removes registry.json"
  fi
  daemon_gone_check "no gateway daemon after brew zap" 15
  if [[ -d "$APP_PATH" ]]; then fail "app removed" "$APP_PATH still exists"; else pass "app removed"; fi
  ;;
esac
finish_step
