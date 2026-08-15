#!/usr/bin/env bash
#
# Regression tests for scripts/install.sh (SBS-740).
#
# install.ps1 has a Pester suite that mocks the network and the installer and
# asserts on the behaviour a user gets. install.sh had no harness, and none of
# its download verification was exercised by CI. This drives the real script
# the same way the Pester suite does: curl is shimmed on PATH to serve a fake
# release JSON and fake download bytes, and the Linux AppImage path (which
# needs no root and touches only $XDG_BIN_HOME) runs for real inside a temp
# HOME. shasum is deliberately NOT mocked: the fake release advertises the
# true digest of the bytes curl writes, so the checksum tests fail if
# verification is skipped or inverted.
#
# Run: bash scripts/install.Tests.bash

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
INSTALL_SH="$SCRIPT_DIR/install.sh"

pass=0
fail=0

# 64 known bytes stand in for the downloaded AppImage. The fake release
# advertises this exact size and digest.
fake_bytes=""
for i in $(seq 1 64); do fake_bytes="${fake_bytes}\\x$(printf '%02x' "$i")"; done
fake_sha256="$(printf '%b' "$fake_bytes" | shasum -a 256 | awk '{print $1}')"

# Pretty-printed shape the script's grep/sed/awk parsers expect.
fake_release() {
  local digest="$1" size="$2"
  local url="${3:-https://github.com/tsouth89/toolport/releases/download/v1.13.0/Toolport_1.13.0_amd64.AppImage}"
  cat <<EOF
{
  "tag_name": "v1.13.0",
  "assets": [
    {
      "url": "https://api.github.com/repos/tsouth89/toolport/releases/assets/1",
      "id": 1,
      "name": "Toolport_1.13.0_amd64.AppImage",
      "state": "uploaded",
      "download_count": 0,
      "browser_download_url": "$url",
      "size": $size,
      "digest": "$digest"
    }
  ]
}
EOF
}

# Build a shim bin dir: curl serves the fake release for the API call and writes
# fake bytes for asset downloads; uname reports Linux x86_64 so the script takes
# the no-root AppImage path. The release JSON lives in its own file (embedded
# into the shim would need sed tricks that differ between BSD and GNU).
make_shim() {
  local shim_dir="$1" release_json="$2"
  mkdir -p "$shim_dir"
  printf '%s' "$release_json" > "$shim_dir/release.json"
  cat > "$shim_dir/curl" <<'EOF'
#!/usr/bin/env bash
# Args: --proto =https -fsSL <url> -o <dest>   (download)  |  -fsSL <api-url>  (lookup)
has_out=0
dest=""
for arg in "$@"; do
  if [ "$has_out" = "1" ]; then dest="$arg"; break; fi
  if [ "$arg" = "-o" ]; then has_out=1; fi
done
if [ -n "$dest" ]; then
  if [ "${TOOLPORT_TEST_EMPTY_DOWNLOAD:-0}" = "1" ]; then
    : > "$dest"
    exit 0
  fi
  if [ "${TOOLPORT_TEST_CURL_FAIL:-0}" = "1" ]; then
    printf "partial" > "$dest"
    exit 1
  fi
  printf "%b" "\\x01\\x02\\x03\\x04\\x05\\x06\\x07\\x08\\x09\\x0a\\x0b\\x0c\\x0d\\x0e\\x0f\\x10\\x11\\x12\\x13\\x14\\x15\\x16\\x17\\x18\\x19\\x1a\\x1b\\x1c\\x1d\\x1e\\x1f\\x20\\x21\\x22\\x23\\x24\\x25\\x26\\x27\\x28\\x29\\x2a\\x2b\\x2c\\x2d\\x2e\\x2f\\x30\\x31\\x32\\x33\\x34\\x35\\x36\\x37\\x38\\x39\\x3a\\x3b\\x3c\\x3d\\x3e\\x3f\\x40" > "$dest"
  exit 0
fi
dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cat "$dir/release.json"
EOF
  chmod +x "$shim_dir/curl"
  cat > "$shim_dir/uname" <<'EOF'
#!/usr/bin/env bash
case "$1" in
  -s) echo Linux ;;
  -m) echo x86_64 ;;
esac
EOF
  chmod +x "$shim_dir/uname"
}

# run_install <release-json> [ENV=value ...] -> prints "<rc>\t<output>"
run_install() {
  local release_json="$1"
  shift
  local workdir shim home bindir
  workdir="$(mktemp -d)"
  shim="$workdir/shim"; home="$workdir/home"; bindir="$workdir/bin"
  mkdir -p "$home" "$bindir"
  make_shim "$shim" "$release_json"
  local output rc
  set +e
  output="$(cd "$workdir" && PATH="$shim:$PATH" HOME="$home" XDG_BIN_HOME="$bindir" env "$@" bash "$INSTALL_SH" 2>&1)"
  rc=$?
  set -e
  printf '%s\t%s' "$rc" "$output"
}

check() {
  local name="$1" expected="$2" actual="$3"
  if [[ "$actual" == *"$expected"* ]]; then
    echo "  ok: $name"
    pass=$((pass + 1))
  else
    echo "  FAIL: $name (expected output containing: $expected)"
    fail=$((fail + 1))
  fi
}

# run_check <label> <expected> <want-rc> <release-json> [ENV=value ...]
run_check() {
  local label="$1" expected="$2" want_rc="$3" release_json="$4"
  shift 4
  local result rc out
  result="$(run_install "$release_json" "$@")"
  rc="${result%%$'\t'*}"
  out="${result#*$'\t'}"
  check "$label" "$expected" "$out"
  if [[ "$rc" == "$want_rc" ]]; then
    echo "  ok: exit code $want_rc"; pass=$((pass + 1))
  else
    echo "  FAIL: exit code $rc (wanted $want_rc)"; fail=$((fail + 1))
  fi
  echo "    output: $(printf '%s' "$out" | tr '\n' ' ' | cut -c1-150)"
}

echo "== install.sh download verification =="

echo "happy path (digest verified, AppImage installed)"
run_check "verifies and reports the digest" "sha256 verified: $fake_sha256" 0 "$(fake_release "sha256:$fake_sha256" 64)"
run_check "installs the AppImage" "Installed the AppImage" 0 "$(fake_release "sha256:$fake_sha256" 64)"

echo "digest mismatch (refuses before install)"
run_check "reports the mismatch" "mismatch for Toolport_1.13.0_amd64.AppImage" 1 "$(fake_release "sha256:$(printf '0%.0s' $(seq 1 64))" 64)"

echo "digest mismatch (working install preserved)"
workdir="$(mktemp -d)"
shim="$workdir/shim"; home="$workdir/home"; bindir="$workdir/bin"
mkdir -p "$home" "$bindir"
printf 'existing working install' > "$bindir/toolport"
make_shim "$shim" "$(fake_release "sha256:$(printf '0%.0s' $(seq 1 64))" 64)"
set +e
mismatch_output="$(cd "$workdir" && PATH="$shim:$PATH" HOME="$home" XDG_BIN_HOME="$bindir" bash "$INSTALL_SH" 2>&1)"
mismatch_rc=$?
set -e
if [ "$mismatch_rc" = "1" ] && [ "$(cat "$bindir/toolport")" = "existing working install" ]; then
  echo "  ok: digest mismatch leaves the working install untouched"; pass=$((pass + 1))
else
  echo "  FAIL: digest mismatch clobbered the working install (rc=$mismatch_rc, content: $(cat "$bindir/toolport"))"; fail=$((fail + 1))
fi
rm -rf "$workdir"

echo "no digest (refuses by default)"
run_check "refuses with the opt-out hint" "TOOLPORT_ALLOW_UNVERIFIED=1" 1 "$(fake_release "" 64)"

echo "no digest (allowed with TOOLPORT_ALLOW_UNVERIFIED=1)"
run_check "warns about unverified install" "installing unverified" 0 "$(fake_release "" 64)" TOOLPORT_ALLOW_UNVERIFIED=1

echo "size mismatch (treated as truncated)"
run_check "reports truncation" "Treating it as truncated" 1 "$(fake_release "sha256:$fake_sha256" 99999)"

echo "non-https URL (refused before download)"
run_check "refuses a non-https URL" "non-https URL" 1 "$(fake_release "sha256:$fake_sha256" 64 "http://example.invalid/Toolport_1.13.0_amd64.AppImage")"

echo "empty download (rejected, destination removed)"
run_check "rejects the empty download" "empty file" 1 "$(fake_release "sha256:$fake_sha256" 64)" TOOLPORT_TEST_EMPTY_DOWNLOAD=1

echo "curl failure (partial download removed, AppImage path)"
workdir="$(mktemp -d)"
shim="$workdir/shim"; home="$workdir/home"; bindir="$workdir/bin"
mkdir -p "$home" "$bindir"
make_shim "$shim" "$(fake_release "sha256:$fake_sha256" 64)"
set +e
curl_fail_output="$(cd "$workdir" && PATH="$shim:$PATH" HOME="$home" XDG_BIN_HOME="$bindir" TOOLPORT_TEST_CURL_FAIL=1 bash "$INSTALL_SH" 2>&1)"
curl_fail_rc=$?
set -e
check "reports the download failure" "Download failed" "$curl_fail_output"
if [ "$curl_fail_rc" = "1" ]; then
  echo "  ok: exit code 1"; pass=$((pass + 1))
else
  echo "  FAIL: exit code $curl_fail_rc (wanted 1)"; fail=$((fail + 1))
fi
if [ ! -e "$bindir/toolport" ]; then
  echo "  ok: partial AppImage removed"; pass=$((pass + 1))
else
  echo "  FAIL: partial AppImage left behind"; fail=$((fail + 1))
fi
echo "    output: $(printf '%s' "$curl_fail_output" | tr '\n' ' ' | cut -c1-150)"
rm -rf "$workdir"

echo
echo "$pass passed, $fail failed"
[ "$fail" = "0" ]
