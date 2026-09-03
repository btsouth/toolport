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

# The Debian release: same bytes, advertised as the .deb the apt path selects.
fake_release_deb() {
  local digest="$1" size="$2"
  fake_release "$digest" "$size" \
    "https://github.com/tsouth89/toolport/releases/download/v1.13.0/Toolport_1.13.0_amd64.deb" |
    sed 's/Toolport_1\.13\.0_amd64\.AppImage/Toolport_1.13.0_amd64.deb/'
}

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

  # Shadow pacman and every AUR helper install.sh knows about.
  #
  # Without this, running these tests on an Arch or Manjaro box finds the REAL
  # pacman and the REAL paru/yay/pamac on PATH, and the Arch branch then builds
  # and sudo-installs toolport-bin from the actual AUR in the middle of a unit
  # test. CI runs this file on Ubuntu, where there is no pacman to find, so
  # that hole only ever opens on the maintainer's own machine, which is the
  # worst place for it.
  #
  # The Debian branch installs the .deb and the Arch branch installs the native
  # package; both return instead of falling through to the AppImage, so these
  # tests must be able to say which kind of host they are simulating. install.sh
  # reads TOOLPORT_OS_RELEASE, and every test gets a distribution with neither
  # dpkg nor pacman semantics by default; the Debian and Arch tests below point
  # at their own. Without this the file cannot pass on a maintainer's own Arch
  # box, where the real pacman is on PATH and cannot be shadowed away, nor on
  # the Ubuntu CI runner, where dpkg and apt-get are.
  printf 'ID=fedora\n' > "$shim_dir/os-release-generic"
  printf 'ID=ubuntu\nID_LIKE=debian\n' > "$shim_dir/os-release-debian"
  printf 'ID=omarchy\nID_LIKE=arch\n' > "$shim_dir/os-release-arch"

  # Shadowing the AUR helpers still matters on an Arch box: without it a stray
  # code path could reach the real paru/yay in the middle of a unit test.
  local helper
  for helper in paru yay pamac pikaur trizen omarchy; do
    cat > "$shim_dir/$helper" <<EOF
#!/usr/bin/env bash
me=\$(basename "\$0")
printf '%s %s\n' "\$me" "\$*" >> "$shim_dir/helper-argv.log"
# Whatever this reads from stdin is what a real helper's prompt would have
# swallowed. install.sh must hand it /dev/null, so this must stay empty.
cat >> "$shim_dir/helper-stdin.log"
[ "\${TOOLPORT_TEST_AUR_HELPER:-}" = "\$me" ] && exit 0
exit 1
EOF
    chmod +x "$shim_dir/$helper"
  done
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
  # </dev/null, not inherited stdin: the helper shims read stdin to prove
  # install.sh never lets them, and an inherited terminal would block them
  # forever. Only the Arch test below pipes anything, which is what makes its
  # stdin assertion falsifiable while these stay deterministic.
  output="$(cd "$workdir" && PATH="$shim:$PATH" HOME="$home" XDG_BIN_HOME="$bindir" \
    TOOLPORT_OS_RELEASE="$shim/os-release-generic" env "$@" bash "$INSTALL_SH" </dev/null 2>&1)"
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
mismatch_output="$(cd "$workdir" && PATH="$shim:$PATH" HOME="$home" XDG_BIN_HOME="$bindir" TOOLPORT_OS_RELEASE="$shim/os-release-generic" bash "$INSTALL_SH" </dev/null 2>&1)"
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
curl_fail_output="$(cd "$workdir" && PATH="$shim:$PATH" HOME="$home" XDG_BIN_HOME="$bindir" \
  TOOLPORT_OS_RELEASE="$shim/os-release-generic" TOOLPORT_TEST_CURL_FAIL=1 bash "$INSTALL_SH" </dev/null 2>&1)"
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

echo "Arch: adds the signed repository and installs the native package"
# Since 1.18.0 Arch gets the native GTK package from Toolport's own pacman repo,
# so updates arrive with an ordinary `pacman -Syu` instead of a Toolport-specific
# command or a self-updater fighting the package manager. The installer must add
# the repo, pin-trust the signing key, install, and never touch the AppImage or
# reach for an AUR helper on the user's behalf.
arch_workdir="$(mktemp -d)"
arch_shim="$arch_workdir/shim"; arch_home="$arch_workdir/home"; arch_bin="$arch_workdir/bin"
mkdir -p "$arch_home" "$arch_bin"
make_shim "$arch_shim" "$(fake_release "sha256:$fake_sha256" 64)"

# pacman-key and sudo are recorded, not executed: this asserts what the installer
# asks the system to do without letting it touch the real keyring or /etc.
arch_conf="$arch_workdir/pacman.conf"
: > "$arch_conf"
cat > "$arch_shim/pacman-key" <<EOF
#!/usr/bin/env bash
printf 'pacman-key %s\n' "\$*" >> "$arch_shim/pacman-key.log"
EOF
cat > "$arch_shim/sudo" <<'EOF'
#!/usr/bin/env bash
exec "$@"
EOF
cat > "$arch_shim/pacman" <<EOF
#!/usr/bin/env bash
printf 'pacman %s\n' "\$*" >> "$arch_shim/pacman.log"
EOF
chmod +x "$arch_shim/pacman-key" "$arch_shim/sudo" "$arch_shim/pacman"

set +e
arch_output="$(cd "$arch_workdir" && PATH="$arch_shim:$PATH" HOME="$arch_home" \
  XDG_BIN_HOME="$arch_bin" TOOLPORT_OS_RELEASE="$arch_shim/os-release-arch" \
  TOOLPORT_PACMAN_CONF="$arch_conf" TOOLPORT_REPO_KEY_ID="DEADBEEFDEADBEEF" \
  bash "$INSTALL_SH" 2>&1)"
arch_rc=$?
set -e
arch_argv="$(cat "$arch_shim/helper-argv.log" 2>/dev/null || true)"

if [ "$arch_rc" = "0" ]; then
  echo "  ok: exit code 0"; pass=$((pass + 1))
else
  echo "  FAIL: exit code $arch_rc (wanted 0)"; fail=$((fail + 1))
fi
check "installs the native package" "Installing toolport" "$arch_output"
check "explains how updates arrive" "pacman -Syu" "$arch_output"
if grep -q '^\[toolport\]' "$arch_conf" 2>/dev/null; then
  echo "  ok: repository added to pacman.conf"; pass=$((pass + 1))
else
  echo "  FAIL: no [toolport] section in pacman.conf"; fail=$((fail + 1))
fi
# The pin is the security property: --lsign-key must name the expected
# fingerprint, so a key swapped at the host cannot become trusted.
if grep -q -- "--lsign-key DEADBEEFDEADBEEF" "$arch_shim/pacman-key.log" 2>/dev/null; then
  echo "  ok: trusted only the pinned fingerprint"; pass=$((pass + 1))
else
  echo "  FAIL: did not lsign the pinned key id"; fail=$((fail + 1))
fi
if grep -q -- "-Sy .*toolport" "$arch_shim/pacman.log" 2>/dev/null; then
  echo "  ok: installed toolport with pacman"; pass=$((pass + 1))
else
  echo "  FAIL: pacman was not asked to install toolport"; fail=$((fail + 1))
fi
if [ -z "$arch_argv" ]; then
  echo "  ok: no AUR helper was invoked"; pass=$((pass + 1))
else
  echo "  FAIL: an AUR helper ran: $(printf '%s' "$arch_argv" | head -c 80)"; fail=$((fail + 1))
fi
if [ -e "$arch_bin/toolport" ]; then
  echo "  FAIL: installed an AppImage on Arch"; fail=$((fail + 1))
else
  echo "  ok: no AppImage on Arch"; pass=$((pass + 1))
fi
# A second run must be idempotent: the repo block is added once, not appended
# on every install.
(cd "$arch_workdir" && PATH="$arch_shim:$PATH" HOME="$arch_home" \
  XDG_BIN_HOME="$arch_bin" TOOLPORT_OS_RELEASE="$arch_shim/os-release-arch" \
  TOOLPORT_PACMAN_CONF="$arch_conf" TOOLPORT_REPO_KEY_ID="DEADBEEFDEADBEEF" \
  bash "$INSTALL_SH" >/dev/null 2>&1) || true
if [ "$(grep -c '^\[toolport\]' "$arch_conf")" = "1" ]; then
  echo "  ok: repository block added once, not per run"; pass=$((pass + 1))
else
  echo "  FAIL: pacman.conf gained a duplicate [toolport] section"; fail=$((fail + 1))
fi
rm -rf "$arch_workdir"

echo "Debian: verifies the .deb and installs it with apt"
# The apt path is the one most Linux users hit. It must select the .deb, verify
# it like any other download, hand it to apt-get and never place an AppImage.
# dpkg only has to exist; apt-get is recorded, not run; sudo passes through.
deb_workdir="$(mktemp -d)"
deb_shim="$deb_workdir/shim"; deb_home="$deb_workdir/home"; deb_bin="$deb_workdir/bin"
mkdir -p "$deb_home" "$deb_bin"
make_shim "$deb_shim" "$(fake_release_deb "sha256:$fake_sha256" 64)"
printf '#!/usr/bin/env bash\nexit 0\n' > "$deb_shim/dpkg"
cat > "$deb_shim/apt-get" <<EOF
#!/usr/bin/env bash
printf 'apt-get %s\n' "\$*" >> "$deb_shim/apt-get.log"
EOF
cat > "$deb_shim/sudo" <<'EOF'
#!/usr/bin/env bash
exec "$@"
EOF
chmod +x "$deb_shim/dpkg" "$deb_shim/apt-get" "$deb_shim/sudo"

set +e
deb_output="$(cd "$deb_workdir" && PATH="$deb_shim:$PATH" HOME="$deb_home" \
  XDG_BIN_HOME="$deb_bin" TOOLPORT_OS_RELEASE="$deb_shim/os-release-debian" \
  bash "$INSTALL_SH" </dev/null 2>&1)"
deb_rc=$?
set -e

if [ "$deb_rc" = "0" ]; then
  echo "  ok: exit code 0"; pass=$((pass + 1))
else
  echo "  FAIL: exit code $deb_rc (wanted 0)"; fail=$((fail + 1))
  echo "    output: $deb_output"
fi
check "verifies the .deb digest" "sha256 verified: $fake_sha256" "$deb_output"
check "installs with apt" "Installing with apt" "$deb_output"
if grep -q -- "install -y .*toolport\.deb" "$deb_shim/apt-get.log" 2>/dev/null; then
  echo "  ok: apt-get was asked to install the .deb"; pass=$((pass + 1))
else
  echo "  FAIL: apt-get was not asked to install the .deb"; fail=$((fail + 1))
fi
if [ ! -e "$deb_bin/toolport" ]; then
  echo "  ok: no AppImage on Debian"; pass=$((pass + 1))
else
  echo "  FAIL: an AppImage was installed on Debian"; fail=$((fail + 1))
fi
rm -rf "$deb_workdir"

echo
echo "$pass passed, $fail failed"
[ "$fail" = "0" ]
