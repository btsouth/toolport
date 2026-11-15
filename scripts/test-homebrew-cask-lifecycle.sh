#!/usr/bin/env bash
# Real Homebrew against the repo's cask (packaging/homebrew/toolport.rb), with
# only version, url and sha256 swapped for a local stub dmg. The stub's
# toolport-gateway logs every invocation, so this shows exactly what each brew
# command runs:
#   brew reinstall  -> nothing (clients stay connected)
#   brew upgrade    -> nothing
#   brew uninstall  -> --disconnect-all --uninstall
#   brew uninstall --zap -> --remove-data --confirm
# The cask needs both its signals (call stack and brew command) to agree before
# it disconnects anything; the test-only HOMEBREW_TOOLPORT_CASK_TEST_IGNORE_*
# variables hide one to check that a disagreement runs nothing and says so.
# CI only: it installs into /Applications and touches ~/Library like a user would.
set -euo pipefail
[[ "${CI:-}" == true ]] || { echo "CI only: this installs a stub Toolport.app into /Applications" >&2; exit 2; }
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
export HOMEBREW_NO_AUTO_UPDATE=1 HOMEBREW_NO_INSTALL_CLEANUP=1 HOMEBREW_NO_ANALYTICS=1 HOMEBREW_NO_ENV_HINTS=1
brew --version
work="$(mktemp -d)"
calls="$work/calls.log"
TAP=local/toolportcasktest
CASK="$TAP/toolport"

fail() { echo "FAIL: $*" >&2; echo "--- gateway calls:" >&2; cat "$calls" >&2 || true; exit 1; }

make_dmg() { # make_dmg <version>
  local stage="$work/stage-$1" app
  app="$stage/Toolport.app"
  mkdir -p "$app/Contents/MacOS"
  cat >"$app/Contents/Info.plist" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
  <key>CFBundleIdentifier</key><string>com.tsout.conduit.casktest</string>
  <key>CFBundleName</key><string>Toolport</string>
  <key>CFBundleExecutable</key><string>conduit</string>
  <key>CFBundleShortVersionString</key><string>$1</string>
  <key>CFBundlePackageType</key><string>APPL</string>
</dict></plist>
EOF
  printf '#!/bin/sh\nexit 0\n' >"$app/Contents/MacOS/conduit"
  # Logs its arguments; prints a report for --remove-data like the real one.
  printf '#!/bin/sh\necho "%s $*" >> "%s"\ncase "$*" in *--remove-data*) echo "{\\"leftovers\\":[]}";; esac\nexit 0\n' "$1" "$calls" \
    >"$app/Contents/MacOS/toolport-gateway"
  chmod +x "$app/Contents/MacOS/conduit" "$app/Contents/MacOS/toolport-gateway"
  hdiutil create -quiet -volname Toolport -srcfolder "$stage" -format UDZO -ov "$work/Toolport-$1.dmg"
}

write_cask() { # write_cask <version>
  python3 - "$ROOT/packaging/homebrew/toolport.rb" "$1" "$work/Toolport-$1.dmg" "$CASK_FILE" <<'PY'
import hashlib, re, sys
source, version, dmg, out = sys.argv[1:5]
s = open(source).read()
digest = hashlib.sha256(open(dmg, "rb").read()).hexdigest()
s, n = re.subn(r'^  version "[^"]+"$', f'  version "{version}"', s, count=1, flags=re.M)
assert n == 1
s, n = re.subn(r'  on_arm do\n.*?\n  end\n  on_intel do\n.*?\n  end\n',
               f'  sha256 "{digest}"\n  url "file://{dmg}"\n', s, count=1, flags=re.S)
assert n == 1, "on_arm/on_intel blocks not found"
s = re.sub(r'  livecheck do\n.*?\n  end\n', '', s, count=1, flags=re.S)
open(out, "w").write(s)
PY
}

calls_since() { cat "$calls" 2>/dev/null || true; }
reset_calls() { : >"$calls"; }
warning="Toolport could not tell whether Homebrew is upgrading or removing it"
expect_warning() { # expect_warning <label> <brew output> <manual command>
  grep -qF "$warning" <<<"$2" || fail "$1 did not explain the disagreement: $2"
  grep -qF -- "$3" <<<"$2" || fail "$1 did not show how to run $3: $2"
}

brew tap-new --no-git "$TAP" >/dev/null
CASK_FILE="$(brew --repository "$TAP")/Casks/toolport.rb"
mkdir -p "$(dirname "$CASK_FILE")"
trap 'brew uninstall --cask --force "$CASK" >/dev/null 2>&1 || true; brew untap "$TAP" >/dev/null 2>&1 || true' EXIT

make_dmg 1.0.0
make_dmg 1.0.1
make_dmg 1.0.2
write_cask 1.0.0
brew install --cask "$CASK"
[[ -x /Applications/Toolport.app/Contents/MacOS/toolport-gateway ]] || fail "install did not place Toolport.app"

# Data a plain uninstall must keep.
data="$HOME/Library/Application Support/Toolport"
mkdir -p "$data"
echo '{"servers":[]}' >"$data/registry.json"

reset_calls
brew reinstall --cask "$CASK"
[[ -z "$(calls_since)" ]] || fail "brew reinstall ran the gateway: $(calls_since)"
echo "PASS: brew reinstall leaves clients connected (gateway not run)"

reset_calls
out="$(HOMEBREW_TOOLPORT_CASK_TEST_IGNORE_STACK=1 brew reinstall --cask "$CASK" 2>&1)" || fail "reinstall failed: $out"
echo "$out"
[[ -z "$(calls_since)" ]] || fail "brew reinstall without the stack signal ran the gateway: $(calls_since)"
expect_warning "brew reinstall without the stack signal" "$out" "--disconnect-all --uninstall"
echo "PASS: brew reinstall with only the command signal runs nothing and warns"

reset_calls
write_cask 1.0.1
brew upgrade --cask "$CASK"
grep -q 'CFBundleShortVersionString</key><string>1.0.1' /Applications/Toolport.app/Contents/Info.plist || fail "upgrade did not install 1.0.1"
[[ -z "$(calls_since)" ]] || fail "brew upgrade ran the gateway: $(calls_since)"
echo "PASS: brew upgrade leaves clients connected (gateway not run)"

reset_calls
write_cask 1.0.2
out="$(HOMEBREW_TOOLPORT_CASK_TEST_IGNORE_STACK=1 brew upgrade --cask "$CASK" 2>&1)" || fail "upgrade failed: $out"
echo "$out"
grep -q 'CFBundleShortVersionString</key><string>1.0.2' /Applications/Toolport.app/Contents/Info.plist || fail "upgrade did not install 1.0.2"
[[ -z "$(calls_since)" ]] || fail "brew upgrade without the stack signal ran the gateway: $(calls_since)"
expect_warning "brew upgrade without the stack signal" "$out" "--disconnect-all --uninstall"
echo "PASS: brew upgrade with only the command signal runs nothing and warns"

reset_calls
out="$(HOMEBREW_TOOLPORT_CASK_TEST_IGNORE_COMMAND=1 brew uninstall --cask "$CASK" 2>&1)" || fail "uninstall failed: $out"
echo "$out"
[[ -z "$(calls_since)" ]] || fail "brew uninstall without the command signal ran the gateway: $(calls_since)"
expect_warning "brew uninstall without the command signal" "$out" "toolport-gateway\" --disconnect-all --uninstall"
[[ ! -d /Applications/Toolport.app ]] || fail "brew uninstall without the command signal left the app"
echo "PASS: brew uninstall with only the stack signal runs nothing and says how to finish"
brew install --cask "$CASK"

reset_calls
brew uninstall --cask "$CASK"
[[ "$(calls_since)" == "1.0.2 --disconnect-all --uninstall" ]] || fail "brew uninstall ran: $(calls_since)"
[[ -f "$data/registry.json" ]] || fail "brew uninstall removed registry.json"
[[ ! -d /Applications/Toolport.app ]] || fail "brew uninstall left the app"
echo "PASS: brew uninstall restores clients and removes launch at login, keeps data"

brew install --cask "$CASK"
reset_calls
rm -f "$data/registry.json"   # the stub does not remove data; let rmdir see an empty folder
brew uninstall --cask --zap "$CASK"
[[ "$(calls_since)" == "1.0.2 --remove-data --confirm" ]] || fail "brew uninstall --zap ran: $(calls_since)"
[[ -f "$HOME/Toolport-removal-report.json" ]] || fail "zap did not save the removal report"
[[ ! -d "$data" ]] || fail "zap left the emptied data folder"
echo "PASS: brew uninstall --zap runs Toolport's data removal and removes the emptied folder"
