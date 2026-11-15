#!/usr/bin/env bash
# Signature, notarization, Gatekeeper and architecture checks for a Toolport.app.
#
# usage: bundle-checks.sh <path/to/Toolport.app> <expected-arch: arm64|x86_64> [label]
# env:   SIGNED=0 marks signing checks as not applicable (local unsigned builds);
#        published releases always run with SIGNED=1.
set -uo pipefail
APP="$1"
WANT_ARCH="$2"
AREA="bundle${3:+ ($3)}"
# shellcheck source=common.sh
source "$(dirname "$0")/common.sh"
SIGNED="${SIGNED:-1}"
TEAM_ID="V4YZPC7T6G"
GROUP="$TEAM_ID.com.tsout.conduit.shared"
HELPER="$APP/Contents/Helpers/ToolportGateway.app"

[[ -d "$APP" ]] || { fail "app bundle exists" "$APP missing"; finish_step; }
version="$(/usr/libexec/PlistBuddy -c 'Print :CFBundleShortVersionString' "$APP/Contents/Info.plist" 2>/dev/null)"
if [[ -n "$version" ]]; then pass "bundle version readable" "$version"; else fail "bundle version readable" "no CFBundleShortVersionString"; fi
info "note: minimum macOS" "$(/usr/libexec/PlistBuddy -c 'Print :LSMinimumSystemVersion' "$APP/Contents/Info.plist" 2>/dev/null || echo unset)"

if [[ "$SIGNED" == "1" ]]; then
  check "codesign --verify --deep --strict" codesign --verify --deep --strict --verbose=2 "$APP"
  # Gatekeeper's verdict for a downloaded app. Must say Notarized Developer ID.
  spctl -a -vv -t exec "$APP" >"$EVIDENCE/logs/spctl-$WANT_ARCH.log" 2>&1
  rc=$?
  verdict="$(tr '\n' ' ' <"$EVIDENCE/logs/spctl-$WANT_ARCH.log")"
  if [[ $rc -eq 0 ]] && grep -q 'source=Notarized Developer ID' "$EVIDENCE/logs/spctl-$WANT_ARCH.log"; then
    pass "Gatekeeper accepts (spctl -a -vv)" "$verdict"
  else
    fail "Gatekeeper accepts (spctl -a -vv)" "exit $rc: $verdict"
  fi
  check "notarization ticket stapled (app)" xcrun stapler validate "$APP"
  if [[ -d "$HELPER" ]]; then
    check "codesign --verify --strict (gateway helper)" codesign --verify --strict --verbose=2 "$HELPER"
  else
    fail "nested gateway helper exists" "$HELPER missing"
  fi
  if command -v syspolicy_check >/dev/null 2>&1; then
    check "syspolicy_check distribution" syspolicy_check distribution "$APP"
  else
    untested "syspolicy_check distribution" "tool not present on this macOS"
  fi

  codesign -dvvv "$APP" >"$EVIDENCE/logs/codesign-d-app-$WANT_ARCH.log" 2>&1
  d="$EVIDENCE/logs/codesign-d-app-$WANT_ARCH.log"
  if grep -q "^TeamIdentifier=$TEAM_ID" "$d" && grep -q '^Authority=Developer ID Application' "$d"; then
    pass "signed by Developer ID $TEAM_ID" "$(grep '^Authority=' "$d" | head -1)"
  else
    fail "signed by Developer ID $TEAM_ID" "$(grep -E '^(Authority|TeamIdentifier)=' "$d" | tr '\n' ' ')"
  fi
  if grep -Eq 'flags=.*runtime' "$d"; then pass "hardened runtime (app)"; else fail "hardened runtime (app)" "$(grep flags= "$d")"; fi
  if grep -q '^Timestamp=' "$d"; then pass "secure timestamp (app)" "$(grep '^Timestamp=' "$d")"; else fail "secure timestamp (app)"; fi

  for b in "$APP" "$HELPER"; do
    name="$(basename "$b")"
    ents="$(codesign -d --entitlements :- "$b" 2>/dev/null)"
    printf '%s\n' "$ents" >"$EVIDENCE/logs/entitlements-$name-$WANT_ARCH.plist"
    if grep -q "$GROUP" <<<"$ents"; then
      pass "keychain access group entitlement ($name)" "$GROUP"
    else
      fail "keychain access group entitlement ($name)" "missing $GROUP"
    fi
    prof="$b/Contents/embedded.provisionprofile"
    if [[ -f "$prof" ]]; then
      exp="$(security cms -D -i "$prof" 2>/dev/null | plutil -extract ExpirationDate raw -o - - 2>/dev/null)"
      exp_s="$(date -j -f '%Y-%m-%dT%H:%M:%SZ' "$exp" +%s 2>/dev/null || echo 0)"
      days=$(( (exp_s - $(date +%s)) / 86400 ))
      if (( days > 30 )); then
        pass "provisioning profile valid ($name)" "expires $exp ($days days)"
      elif (( days > 0 )); then
        fail "provisioning profile valid ($name)" "expires $exp, only $days days left"
      else
        fail "provisioning profile valid ($name)" "expired or unreadable: '$exp'"
      fi
    else
      fail "provisioning profile embedded ($name)" "$prof missing"
    fi
  done
else
  untested "signing, notarization and Gatekeeper" "local unsigned build; only published releases carry Developer ID signatures"
fi

# Gateway entry points clients spawn: the current name and the pre-rename one.
links="toolport-gateway conduit-gateway"
# The pre-rename link is made by the release signing script only.
[[ "$SIGNED" == "1" ]] || links="toolport-gateway"
for link in $links; do
  p="$APP/Contents/MacOS/$link"
  if [[ -x "$p" ]]; then
    pass "client entry point $link resolves" "$(readlink "$p" || echo regular file)"
  else
    fail "client entry point $link resolves" "$p is not executable"
  fi
done

# Every Mach-O in the bundle, its architectures, and its own signature.
machos="$EVIDENCE/logs/macho-$WANT_ARCH.tsv"
: >"$machos"
wrong=0
count=0
while IFS= read -r -d '' f; do
  if file -b "$f" | grep -q 'Mach-O'; then
    count=$((count + 1))
    archs="$(lipo -archs "$f" 2>/dev/null || echo '?')"
    sig="ok"
    if [[ "$SIGNED" == "1" ]] && ! codesign --verify --strict "$f" >/dev/null 2>&1; then sig="UNSIGNED"; fi
    printf '%s\t%s\t%s\n' "${f#"$APP"/}" "$archs" "$sig" >>"$machos"
    if [[ " $archs " != *" $WANT_ARCH "* ]]; then wrong=$((wrong + 1)); fi
    if [[ "$sig" != "ok" ]]; then wrong=$((wrong + 1)); fi
  fi
done < <(find "$APP" -type f -print0)
what="every Mach-O is $WANT_ARCH"
[[ "$SIGNED" == "1" ]] && what="$what and signed"
# The scan must have seen the app's own executable and the gateway, or it
# looked at the wrong thing.
exe="$(/usr/libexec/PlistBuddy -c 'Print :CFBundleExecutable' "$APP/Contents/Info.plist" 2>/dev/null)"
missing=""
# Release signing nests the gateway in its own helper app; an unsigned build
# from a ref keeps it next to the app executable.
gateway_path="Contents/Helpers/ToolportGateway.app/Contents/MacOS/toolport-gateway"
[[ "$SIGNED" == "1" ]] || gateway_path="Contents/MacOS/toolport-gateway"
for need in "Contents/MacOS/${exe:-conduit}" "$gateway_path"; do
  cut -f1 "$machos" | grep -qxF "$need" || missing+="$need "
done
if (( count == 0 )); then
  fail "$what" "no Mach-O binaries found under $APP"
elif [[ -n "$missing" ]]; then
  fail "$what" "the scan did not see: $missing($count binaries, logs/macho-$WANT_ARCH.tsv)"
elif (( wrong == 0 )); then
  pass "$what" "$count binaries: $(cut -f1,2 "$machos" | tr '\t\n' '= ' | cut -c1-300)"
else
  fail "$what" "$wrong problems; see logs/macho-$WANT_ARCH.tsv"
fi
if cut -f2 "$machos" | grep -q 'x86_64' && cut -f2 "$machos" | grep -q 'arm64'; then
  info "note: universal binaries" "at least one binary is universal"
fi
finish_step
