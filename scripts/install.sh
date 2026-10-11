#!/usr/bin/env bash
#
# Toolport installer. One-liner:
#   curl -fsSL https://toolport.app/install.sh | bash
#
# HEADS UP, IF YOU EDIT THIS FILE: toolport.app/install.sh redirects to a PINNED
# COMMIT of this script, not to main, because it is piped into a shell and a movable
# URL there means anything that lands on main runs on users' machines. A change here
# does NOT reach that URL until INSTALL_SCRIPTS_REF in the site repo's
# worker/index.js is moved to the commit containing it. Ship both, or the fix you
# just made will not reach anyone using the short URL.
#
# Installs the latest signed release for your OS/arch:
#   - Linux (x86_64): the GTK .deb on supported Debian/Ubuntu, else the Tauri AppImage
#     into ~/.local/bin with a desktop entry.
#   - macOS: copies Toolport.app from the signed .dmg into /Applications (Homebrew is
#     the cleaner path, and this script points you there).
# Windows: use scripts/install.ps1 instead.
#
# Every download is verified against the SHA-256 GitHub publishes for that asset
# before it is used, and an https-only URL is required. A release that publishes
# no checksum is refused unless TOOLPORT_ALLOW_UNVERIFIED=1 (mirroring
# scripts/install.ps1's -AllowUnverified).
set -euo pipefail

REPO="btsouth/toolport"
API="https://api.github.com/repos/$REPO/releases/latest"

# The Apple Developer team the macOS builds are signed and notarized under (the
# same identity the release workflow's APPLE_* secrets use). `codesign --verify`
# only proves a bundle satisfies its OWN embedded requirement, so a tampered
# build re-signed with any other Developer ID passes it. This is the value that
# ties the bundle to us, so install_macos checks it explicitly.
EXPECTED_TEAM_ID="V4YZPC7T6G"

say() { printf '\033[1;36m>\033[0m %s\n' "$*"; }
err() {
  printf '\033[1;31mx\033[0m %s\n' "$*" >&2
  exit 1
}
need() { command -v "$1" >/dev/null 2>&1 || err "This installer needs '$1' on your PATH."; }

os="$(uname -s)"
arch="$(uname -m)"

# Read values as data, never execute os-release (also used by fixture tests).
os_value() {
  sed -n "s/^$1=//p" "${TOOLPORT_OS_RELEASE:-/etc/os-release}" 2>/dev/null |
    tr -d "\"'" | head -n1
}

version_at_least() {
  [[ "$1" =~ ^[0-9]+([.][0-9]+)*$ ]] || return 1
  awk -v actual="$1" -v floor="$2" 'BEGIN {
    n = split(actual, a, "."); m = split(floor, b, ".")
    for (i = 1; i <= n || i <= m; i++) {
      if (a[i]+0 > b[i]+0) exit 0
      if (a[i]+0 < b[i]+0) exit 1
    }
    exit 0
  }'
}

# Known releases meet every ABI floor in packaging/linux/native/nfpm.yaml.
# Derivatives can report their Ubuntu base; otherwise inspect apt candidates
# without refreshing repositories or installing anything. Unknown means portable.
deb_supported() {
  local distro version package floor candidate
  distro="$(os_value ID)"
  version="$(os_value VERSION_ID)"
  case "$distro" in
    ubuntu) version_at_least "$version" 24.04; return ;;
    debian) version_at_least "$version" 13; return ;;
  esac
  version="$(os_value UBUNTU_VERSION_ID)"
  if [ -n "$version" ]; then
    version_at_least "$version" 24.04
    return
  fi
  command -v apt-cache >/dev/null 2>&1 || return 1
  for package in libgtk-4-1 libadwaita-1-0 libglib2.0-0t64 libc6; do
    case "$package" in
      libgtk-4-1) floor=4.14 ;;
      libadwaita-1-0) floor=1.5 ;;
      libglib2.0-0t64) floor=2.80 ;;
      libc6) floor=2.39 ;;
    esac
    candidate="$(apt-cache policy "$package" 2>/dev/null | awk '/Candidate:/ {print $2; exit}')"
    [ -n "$candidate" ] && [ "$candidate" != "(none)" ] &&
      dpkg --compare-versions "$candidate" ge "$floor" || return 1
  done
}

linux_plan() {
  linux_package=appimage
  linux_reason=""
  local os_release="${TOOLPORT_OS_RELEASE:-/etc/os-release}"
  if grep -qE '^(ID|ID_LIKE)=.*\b(debian|ubuntu)\b' "$os_release" 2>/dev/null &&
    command -v dpkg >/dev/null 2>&1 && command -v apt-get >/dev/null 2>&1; then
    if deb_supported; then
      linux_package=deb
    else
      linux_reason="Using AppImage: the GTK .deb needs Ubuntu 24.04+/Debian 13+ or equivalent libraries; this system does not meet the detected requirements."
    fi
  elif command -v pacman >/dev/null 2>&1 &&
    grep -qE '^(ID|ID_LIKE)=.*\barch\b' "$os_release" 2>/dev/null; then
    linux_package=pacman
  elif grep -qE '^(ID|ID_LIKE)=.*\b(fedora|rhel|centos)\b' "$os_release" 2>/dev/null; then
    linux_reason="Using AppImage on this RPM distro; the manual GTK RPM needs GTK 4.14+, libadwaita 1.5+, GLib 2.80+ and glibc 2.39+ (older Fedora/RHEL do not meet these requirements)."
  fi
}

# Offline and read-only: useful before installing and in minimal distro containers.
case "${1:-}" in
  --print-plan)
    [ "$os" = Linux ] && [ "$arch" = x86_64 ] || err "Print-plan supports Linux x86_64 only."
    linux_plan
    [ -z "$linux_reason" ] || say "$linux_reason"
    printf 'package=%s\n' "$linux_package"
    exit 0
    ;;
  "") ;;
  *) err "Usage: bash install.sh [--print-plan]" ;;
esac

need curl
# Fetch release metadata once, after print-plan so that mode needs no network.
release_json="$(curl -fsSL "$API")" || err "Couldn't reach the GitHub releases API."
tag_name="$(printf '%s' "$release_json" |
  grep -o '"tag_name": *"[^"]*"' | sed 's/.*: *"\([^"]*\)".*/\1/' | head -n1)"

# Download URL for the asset whose filename ends with the given (regex) suffix.
asset_url() {
  printf '%s' "$release_json" |
    grep -o '"browser_download_url": *"[^"]*"' |
    sed 's/.*: *"\([^"]*\)".*/\1/' |
    grep -E "$1\$" | head -n1
}

# The releases API publishes per-asset `size` and `digest` ("sha256:...")
# fields on each asset object, next to its download URL. Pull the field that
# belongs to the asset whose filename matches the given (regex) suffix, so we
# can verify what we download instead of trusting the wire.
asset_field() {
  suffix="$1"
  field="$2"
  printf '%s' "$release_json" |
    awk -v suffix="$suffix" -v field="$field" '
      /"name":/ {
        name = $0
        sub(/^.*"name": *"/, "", name)
        sub(/".*$/, "", name)
      }
      # Emit from each field block once the asset whose name matches the
      # suffix is the current one. GitHub does not guarantee object key order,
      # so the value must be printed when it is parsed, not when the
      # browser_download_url happens to appear.
      /"size":/ {
        if (name ~ suffix "$" && field == "size") {
          size = $0
          sub(/^.*"size": */, "", size)
          sub(/,.*$/, "", size)
          print size
          exit
        }
      }
      /"digest":/ {
        if (name ~ suffix "$" && field == "digest") {
          digest = $0
          sub(/^.*"digest": *"/, "", digest)
          sub(/".*$/, "", digest)
          print digest
          exit
        }
      }
    '
}

# Mirrors install.ps1's EnvFlag: values "0", "false", "no", "off" mean the
# flag is not set; anything else (including "1") means it is.
env_flag() {
  case "${!1:-}" in
    "" | 0 | false | no | off) return 1 ;;
    *) return 0 ;;
  esac
}

# Download an asset and verify it before the caller uses it. Mirrors
# scripts/install.ps1: https-only URLs, the published per-asset digest checked
# before install, and empty or truncated downloads rejected. Refuses a release
# that publishes no checksum unless TOOLPORT_ALLOW_UNVERIFIED=1.
download_and_verify() {
  url="$1"
  dest="$2"
  digest="$3"
  published_size="$4"

  case "$url" in
    https://*) ;;
    *) err "Refusing to download a non-https URL: $url" ;;
  esac

  algo=""
  expected=""
  if [ -n "$digest" ]; then
    algo="$(printf '%s' "$digest" | sed 's/:.*//')"
    expected="$(printf '%s' "$digest" | sed 's/^[^:]*://')"
    case "$algo" in
      sha256 | sha384 | sha512) : ;;
      *) algo="" ; expected="" ;;  # a digest we don't know how to verify
    esac
  fi

  if [ -z "$algo" ] || [ -z "$expected" ]; then
    if env_flag TOOLPORT_ALLOW_UNVERIFIED; then
      say "GitHub publishes no usable checksum for $(basename "$url");"
      say "installing unverified because TOOLPORT_ALLOW_UNVERIFIED is set."
    else
      err "GitHub publishes no checksum for $(basename "$url"), so this download can't be verified." \
        "Refusing to install it. Either download it yourself from the Releases page," \
        "or re-run with TOOLPORT_ALLOW_UNVERIFIED=1 to install anyway."
    fi
  fi

  say "Downloading $(basename "$url")"
  # --proto '=https' also applies to redirects, so a swapped-out asset URL can't
  # bounce the download to a plaintext endpoint.
  if ! curl --proto '=https' -fsSL "$url" -o "$dest"; then
    rm -f "$dest"
    err "Download failed ($url)."
  fi
  if [ ! -s "$dest" ]; then
    rm -f "$dest"
    err "Download produced an empty file ($url)."
  fi
  if [ -n "$published_size" ] && [ "$published_size" != "0" ]; then
    actual_size="$(wc -c < "$dest" | tr -d ' ')"
    if [ "$actual_size" != "$published_size" ]; then
      rm -f "$dest"
      err "Download is $actual_size bytes but the release says $published_size. Treating it as truncated."
    fi
  fi

  if [ -n "$algo" ]; then
    case "$algo" in
      sha256) sumtool="sha256sum" ;;
      sha384) sumtool="sha384sum" ;;
      sha512) sumtool="sha512sum" ;;
    esac
    if command -v "$sumtool" >/dev/null 2>&1; then
      actual="$("$sumtool" "$dest" | awk '{print $1}')"
    else
      need shasum
      actual="$(shasum -a "${algo#sha}" "$dest" | awk '{print $1}')"
    fi
    if [ "$actual" != "$expected" ]; then
      rm -f "$dest"
      err "$algo mismatch for $(basename "$url"). Deleting the download and stopping." \
        "  expected $expected" \
        "  got      $actual"
    fi
    say "$algo verified: $actual"
  fi
}

# Add the Toolport pacman repository and install the native GTK package.
#
# Deliberately not a self-updater: a packaged app overwriting /usr/bin fights
# pacman and breaks file ownership. Adding a repo once means every later update
# is an ordinary system update.
# Fingerprint of the key that signs the Toolport pacman repository. Pinned so a
# compromised or swapped key at repo.toolport.app cannot be trusted by pacman.
# Replace when the signing key rotates.
REPO_SIGNING_KEY="A16BFA2E1014BD6BD718CC6E6621247E3FFA6AA7"

install_arch_repo() {
  # Overridable so the test suite can point at a scratch pacman.conf and a local
  # key instead of writing to /etc and reaching the network.
  repo_url="${TOOLPORT_REPO_URL:-https://repo.toolport.app}"
  pacman_conf="${TOOLPORT_PACMAN_CONF:-/etc/pacman.conf}"
  keyring_url="$repo_url/toolport.gpg"

  sudo=""
  if [ "$(id -u)" -ne 0 ]; then
    command -v sudo >/dev/null 2>&1 && sudo="sudo" ||
      err "Adding the Toolport repository needs root: re-run as root or install sudo."
  fi

  # The expected fingerprint is pinned here rather than read out of whatever the
  # URL served: --lsign-key only signs the fingerprint it is given, so a key
  # swapped at the host cannot become trusted. Importing it is harmless; trusting
  # it is what this pins.
  repo_key_id="${TOOLPORT_REPO_KEY_ID:-$REPO_SIGNING_KEY}"
  case "$repo_key_id" in
    *REPLACE_ME*|"")
      err "This build of the installer has no repository signing key pinned. Grab the package from the Releases page, or report this."
      ;;
  esac

  say "Importing the Toolport signing key"
  tmpkey="$tmp/toolport.gpg"
  curl -fsSL "$keyring_url" -o "$tmpkey" ||
    err "Could not fetch the signing key from $keyring_url"
  $sudo pacman-key --add "$tmpkey" ||
    err "pacman-key could not import the Toolport signing key."
  $sudo pacman-key --lsign-key "$repo_key_id" ||
    err "pacman-key could not trust $repo_key_id. The key served by $keyring_url is not the one this installer expects."

  if grep -q '^\[toolport\]' "$pacman_conf" 2>/dev/null; then
    say "Toolport repository already configured"
  else
    say "Adding the Toolport repository to $pacman_conf"
    printf '\n[toolport]\nServer = %s/$arch\n' "$repo_url" |
      $sudo tee -a "$pacman_conf" >/dev/null ||
      err "Could not write to $pacman_conf"
  fi

  say "Installing toolport"
  $sudo pacman -Syu --noconfirm toolport ||
    err "pacman could not install toolport. Check the output above."

  say "Installed. Launch Toolport from your app menu, or run: toolport-gtk"
  say "Updates arrive with your normal system update (pacman -Syu)."
}

install_linux() {
  [ "$arch" = "x86_64" ] ||
    err "Linux builds are x86_64 only right now (you're on $arch). Use Development mode or grab a build from the Releases page."
  tmp="$(mktemp -d)"
  trap 'rm -rf "$tmp"' EXIT

  linux_plan
  [ -z "$linux_reason" ] || say "$linux_reason"
  if [ "$linux_package" = deb ]; then
    url="$(asset_url '_amd64[.]deb')"
    [ -n "$url" ] || err "No .deb found in $tag_name."
    digest="$(asset_field '_amd64[.]deb' digest)"
    size="$(asset_field '_amd64[.]deb' size)"
    download_and_verify "$url" "$tmp/toolport.deb" "$digest" "$size"
    # Use sudo only when we aren't already root (root shells / containers have no sudo).
    sudo=""
    if [ "$(id -u)" -ne 0 ]; then
      command -v sudo >/dev/null 2>&1 && sudo="sudo" ||
        err "Installing the .deb needs root: re-run as root or install sudo."
    fi
    say "Installing with apt${sudo:+ (you may be prompted for your password)}"
    $sudo apt-get install -y "$tmp/toolport.deb"
    # The GTK package keeps toolport/conduit CLI aliases for existing installs.
    say "Installed. Launch Toolport from your app menu, or run: toolport"
    return
  fi

  # Preserve the signed pacman repository path on Arch and derivatives.
  if [ "$linux_package" = pacman ]; then
    install_arch_repo
    return
  fi

  url="$(asset_url '_amd64[.]AppImage' || true)"
  [ -n "$url" ] || err "No AppImage found in $tag_name."
  bindir="${XDG_BIN_HOME:-$HOME/.local/bin}"
  mkdir -p "$bindir"
  digest="$(asset_field '_amd64[.]AppImage' digest)"
  size="$(asset_field '_amd64[.]AppImage' size)"
  # Stage in $tmp and only move into the install path after verification, so a
  # corrupt or tampered download can never delete a working install.
  download_and_verify "$url" "$tmp/toolport.AppImage" "$digest" "$size"
  mv "$tmp/toolport.AppImage" "$bindir/toolport"
  chmod +x "$bindir/toolport"

  apps="$HOME/.local/share/applications"
  mkdir -p "$apps"
  cat >"$apps/toolport.desktop" <<EOF
[Desktop Entry]
Name=Toolport
Comment=One local gateway for every MCP server
Exec=$bindir/toolport
Type=Application
Categories=Development;Utility;
Terminal=false
EOF

  say "Installed the AppImage to $bindir/toolport"
  case ":$PATH:" in
    *":$bindir:"*) : ;;
    *) say "Add $bindir to your PATH to run 'toolport' from anywhere." ;;
  esac
}

install_macos() {
  say "Tip: on macOS the cleanest install is Homebrew:"
  say "     brew install --cask btsouth/toolport/toolport"
  case "$arch" in
    arm64 | aarch64) suffix='aarch64-apple-darwin[.]dmg' ;;
    x86_64) suffix='x86_64-apple-darwin[.]dmg' ;;
    *) err "Unsupported macOS arch: $arch" ;;
  esac
  url="$(asset_url "$suffix")"
  [ -n "$url" ] || err "No macOS .dmg found in $tag_name."
  tmp="$(mktemp -d)"
  # Detach and clear the staging copy on ANY exit, not only the paths that
  # remember to. Every `err` below is an `exit 1`, and one that forgot would
  # otherwise leave the disk image mounted (SBS-897).
  trap 'hdiutil detach "$tmp/mnt" >/dev/null 2>&1 || true; rm -rf "$tmp" "/Applications/Toolport.app.new"' EXIT
  digest="$(asset_field "$suffix" digest)"
  size="$(asset_field "$suffix" size)"
  download_and_verify "$url" "$tmp/toolport.dmg" "$digest" "$size"
  say "Mounting and copying Toolport.app to /Applications"
  hdiutil attach -nobrowse -readonly -mountpoint "$tmp/mnt" "$tmp/toolport.dmg" >/dev/null ||
    err "Couldn't mount the disk image."
  app="$(/bin/ls -d "$tmp"/mnt/*.app 2>/dev/null | head -n1 || true)"
  if [ -z "$app" ]; then
    hdiutil detach "$tmp/mnt" >/dev/null 2>&1 || true
    err "No .app found in the disk image."
  fi

  # Verify the signature before anything touches /Applications (SBS-897). The
  # digest check above proves the bytes match what GitHub published; it cannot
  # detect an artifact tampered with BEFORE upload, because GitHub hashes
  # whatever it was given. The signature is the control that covers that case,
  # and this is the only chance to apply it: curl does not write
  # com.apple.quarantine, so Gatekeeper never evaluates the copy on first launch.
  #
  # `codesign --verify` alone is NOT that control. It only proves the bundle
  # satisfies its own embedded requirement, so an attacker who re-signs a
  # tampered build with their OWN Developer ID passes it. The team identifier is
  # what ties the bundle to us, so it is checked explicitly and is the hard gate.
  say "Verifying the app signature"
  need codesign
  if ! codesign_output="$(codesign --verify --deep --strict --verbose=4 "$app" 2>&1)"; then
    hdiutil detach "$tmp/mnt" >/dev/null 2>&1 || true
    err "The app in the disk image failed signature verification:" \
      "  $codesign_output" \
      "Refusing to install it. Download the .dmg yourself from the Releases page if you want to inspect it."
  fi
  signing_team="$(codesign -dv --verbose=4 "$app" 2>&1 |
    sed -n 's/^TeamIdentifier=//p' | head -n1)"
  if [ "$signing_team" != "$EXPECTED_TEAM_ID" ]; then
    hdiutil detach "$tmp/mnt" >/dev/null 2>&1 || true
    err "The app is signed by team '${signing_team:-none}', not Toolport's ($EXPECTED_TEAM_ID)." \
      "A valid signature from someone else is exactly what this check exists to catch." \
      "Refusing to install it."
  fi
  # Notarization, as a warning only. Unlike the team check this depends on
  # machine state (an admin can turn assessment off, and an offline machine
  # cannot reach the notary), so a failure here must not block a bundle we have
  # already proved is ours and intact.
  if command -v spctl >/dev/null 2>&1 &&
    ! spctl_output="$(spctl --assess --type execute "$app" 2>&1)"; then
    say "Note: Gatekeeper assessment did not pass ($spctl_output)."
    say "      The signature and team check above both passed, so continuing."
  fi

  # Stage beside the target, then swap by rename only (SBS-897). The old code
  # ran `rm -rf /Applications/Toolport.app` and only then copied, so under
  # `set -euo pipefail` a cp that failed partway aborted with the working
  # install already deleted. Deleting it just before the `mv` has the same
  # defect in a smaller window: a failed rename leaves NOTHING at the
  # destination. So the live bundle is moved aside, not removed, and is moved
  # back if the rename fails.
  staged="/Applications/Toolport.app.new"
  previous="/Applications/Toolport.app.old"
  rm -rf "$staged" "$previous"
  if ! cp -R "$app" "$staged"; then
    rm -rf "$staged"
    hdiutil detach "$tmp/mnt" >/dev/null 2>&1 || true
    err "Couldn't copy Toolport.app into /Applications. Your existing install is untouched."
  fi
  if [ -e "/Applications/Toolport.app" ] && ! mv "/Applications/Toolport.app" "$previous"; then
    rm -rf "$staged"
    hdiutil detach "$tmp/mnt" >/dev/null 2>&1 || true
    err "Couldn't move the existing Toolport.app aside (is it running?). Your existing install is untouched."
  fi
  if ! mv "$staged" "/Applications/Toolport.app"; then
    # Put the working install back before reporting the failure.
    [ -e "$previous" ] && mv "$previous" "/Applications/Toolport.app" || true
    rm -rf "$staged"
    hdiutil detach "$tmp/mnt" >/dev/null 2>&1 || true
    err "Couldn't install Toolport.app. Your previous install has been restored."
  fi
  rm -rf "$previous"
  hdiutil detach "$tmp/mnt" >/dev/null 2>&1 || true
  say "Installed to /Applications/Toolport.app. Open it from Launchpad or run: open -a Toolport"
}

say "Installing Toolport ${tag_name:-latest}"
case "$os" in
  Linux) install_linux ;;
  Darwin) install_macos ;;
  # The PINNED short URL, never raw.githubusercontent/main: this line is a
  # pipe-into-a-shell instruction like the documented one-liner, so it must go
  # through the same pinned-commit control (SBS-894).
  *) err "Unsupported OS: $os. On Windows, run in PowerShell: irm https://toolport.app/install.ps1 | iex" ;;
esac
