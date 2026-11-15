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
  if grep -q 'Remove Toolport from all clients' "$cask_file"; then
    pass "cask tells users to disconnect clients before uninstall"
  else
    fail "cask tells users to disconnect clients before uninstall" "no caveat; docs/uninstall.md says the cask requires the action before uninstall or zap"
  fi
  zap_data="$(grep -nE 'Application Support/(Toolport|Conduit)' "$cask_file" | tr '\n' ' ')"
  if [[ -z "$zap_data" ]]; then
    pass "cask zap keeps the registry, recovery data and credentials"
  else
    fail "cask zap keeps the registry, recovery data and credentials" "zap trashes the data folder: $zap_data (docs/uninstall.md: zap keeps registry/recovery data)"
  fi
  if check "brew install --cask $TAP_CASK" brew install --cask "$TAP_CASK"; then
    q="$(xattr -p com.apple.quarantine "$APP_PATH" 2>/dev/null)"
    info "note: installed app quarantine" "${q:-none}"
  fi
  brew info --cask toolport >"$EVIDENCE/logs/brew-info.log" 2>&1
  ;;
uninstall)
  MANIFEST="${2:-}"
  # Docs: disconnect clients first (the CLI is the documented route for brew users).
  tmo 120 "$GATEWAY" --disconnect-all >"$EVIDENCE/logs/brew-disconnect-all.json" 2>&1
  rc=$?
  if [[ $rc -eq 0 ]]; then
    pass "--disconnect-all before brew uninstall" "logs/brew-disconnect-all.json"
  elif [[ $rc -eq 142 ]]; then
    fail "--disconnect-all before brew uninstall" "timed out after 120s"
  else
    fail "--disconnect-all before brew uninstall" "exit $rc: $(tr -d '\n' <"$EVIDENCE/logs/brew-disconnect-all.json" | cut -c1-300)"
  fi
  # Same byte-exact restoration check as uninstall.sh.
  [[ -n "$MANIFEST" ]] || { fail "client configs compared after disconnecting" "no manifest given"; finish_step; }
  record_rows homebrew "client configs compared after disconnecting" python3 "$HERE/clients.py" restored "$HOME" "$MANIFEST"
  # shellcheck source=ui.sh
  source "$HERE/ui.sh"
  # docs/uninstall.md: turn off Launch at login before removing the app.
  if [[ "$(login_item_state)" == absent ]]; then
    untested "turning off Launch at login removes the login item" "it was not on (setup could not enable it)"
  else
    ui_activate
    if set_launch_at_login off; then
      pass "turning off Launch at login removes the login item" "$LOGIN_PLIST is gone"
    else
      fail "turning off Launch at login removes the login item" "still there: $(login_item_state)"
    fi
  fi
  quit_check "app quits before uninstall"
  had_registry=0
  [[ -f "$DATA_DIR/registry.json" ]] && had_registry=1
  find "$DATA_DIR" -maxdepth 2 -print >"$EVIDENCE/logs/data-dir-before-zap.txt" 2>&1
  check "brew uninstall --cask --zap toolport" brew uninstall --cask --zap toolport
  if [[ $had_registry -eq 0 ]]; then
    untested "zap keeps registry.json" "no registry existed before zap"
  elif [[ -f "$DATA_DIR/registry.json" ]]; then
    pass "zap keeps registry.json, as docs/uninstall.md says"
  else
    fail "zap keeps registry.json, as docs/uninstall.md says" "$DATA_DIR/registry.json was deleted by zap (logs/data-dir-before-zap.txt lists what was there)"
  fi
  if [[ -d "$APP_PATH" ]]; then fail "app removed" "$APP_PATH still exists"; else pass "app removed"; fi
  ;;
esac
finish_step
