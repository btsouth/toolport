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
#   - Linux (x86_64): the .deb via apt where available, else the portable AppImage
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

REPO="tsouth89/toolport"
API="https://api.github.com/repos/$REPO/releases/latest"

say() { printf '\033[1;36m>\033[0m %s\n' "$*"; }
err() {
  printf '\033[1;31mx\033[0m %s\n' "$*" >&2
  exit 1
}
need() { command -v "$1" >/dev/null 2>&1 || err "This installer needs '$1' on your PATH."; }

need curl
os="$(uname -s)"
arch="$(uname -m)"

# Fetch the latest-release metadata once (unauthenticated API is rate-limited, so don't
# hammer it), then resolve pieces out of the JSON with grep/sed (no jq dependency).
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

install_linux() {
  [ "$arch" = "x86_64" ] ||
    err "Linux builds are x86_64 only right now (you're on $arch). Use Development mode or grab a build from the Releases page."
  tmp="$(mktemp -d)"
  trap 'rm -rf "$tmp"' EXIT

  # Prefer the .deb on Debian/Ubuntu: it links the system WebKitGTK and is the most
  # reliable package (see README). Fall back to the no-root AppImage everywhere else.
  if command -v dpkg >/dev/null 2>&1 && command -v apt-get >/dev/null 2>&1; then
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
    # The .deb still ships the crate binary as `conduit` and adds a `toolport`
    # wrapper on PATH (see packaging/linux/deb/toolport). AppImage below is
    # installed as `$bindir/toolport` as well.
    say "Installed. Launch Toolport from your app menu, or run: toolport"
    return
  fi

  url="$(asset_url '_amd64[.]AppImage')"
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
  say "     brew install --cask tsouth89/toolport/toolport"
  case "$arch" in
    arm64 | aarch64) suffix='aarch64-apple-darwin[.]dmg' ;;
    x86_64) suffix='x86_64-apple-darwin[.]dmg' ;;
    *) err "Unsupported macOS arch: $arch" ;;
  esac
  url="$(asset_url "$suffix")"
  [ -n "$url" ] || err "No macOS .dmg found in $tag_name."
  tmp="$(mktemp -d)"
  trap 'rm -rf "$tmp"' EXIT
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
  rm -rf "/Applications/Toolport.app"
  cp -R "$app" /Applications/
  hdiutil detach "$tmp/mnt" >/dev/null 2>&1 || true
  say "Installed to /Applications/Toolport.app. Open it from Launchpad or run: open -a Toolport"
}

say "Installing Toolport ${tag_name:-latest}"
case "$os" in
  Linux) install_linux ;;
  Darwin) install_macos ;;
  *) err "Unsupported OS: $os. On Windows, run in PowerShell: irm https://raw.githubusercontent.com/$REPO/main/scripts/install.ps1 | iex" ;;
esac
