#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
work_root="$(mktemp -d /tmp/toolport-package-lifecycle.XXXXXX)"
trap 'rm -rf "$work_root"' EXIT

for command in fakeroot pacman bsdtar sha256sum zip; do
  command -v "$command" >/dev/null || {
    echo "error: $command is required for the package lifecycle test" >&2
    exit 1
  }
done

stage_root="$work_root/stage"
root="$work_root/root"
database="$work_root/database"
cache="$work_root/cache"
mkdir -p \
  "$root/home/test/.config/Toolport" \
  "$root/home/test/.config/Claude" \
  "$database/local" \
  "$cache"

"$repo_root/scripts/stage-linux-native.sh" "$stage_root"

printf '{"servers":[{"id":"preserve-me"}]}\n' \
  > "$root/home/test/.config/Toolport/registry.json"
printf '{"mcpServers":{"unrelated":{"command":"keep"}}}\n' \
  > "$root/home/test/.config/Claude/claude_desktop_config.json"
before="$(sha256sum \
  "$root/home/test/.config/Toolport/registry.json" \
  "$root/home/test/.config/Claude/claude_desktop_config.json")"

printf '[options]\nArchitecture = auto\nSigLevel = Never\nLocalFileSigLevel = Never\n' \
  > "$work_root/pacman.conf"

build_package() {
  local version="$1"
  local package_root="$work_root/package-$version"
  local package_path="$work_root/toolport-$version.pkg.tar.zst"
  mkdir -p "$package_root"
  cp -a "$stage_root"/. "$package_root"/
  printf 'pkgname = toolport\npkgbase = toolport\npkgver = %s-1\npkgdesc = Native MCP gateway manager for AI coding agents\nurl = https://github.com/tsouth89/toolport\nbuilddate = 0\npackager = Toolport lifecycle smoke\nsize = 1\narch = x86_64\nlicense = MIT\nprovides = toolport\nconflict = toolport-bin\nconflict = toolport-native-preview\nreplaces = toolport-bin\nreplaces = toolport-native-preview\n' \
    "$version" > "$package_root/.PKGINFO"
  if [[ "$version" == "1.17.1" ]]; then
    printf 'X-Toolport-Lifecycle=upgrade\n' \
      >> "$package_root/usr/share/applications/app.toolport.Toolport.desktop"
  fi
  (cd "$package_root" && bsdtar --zstd -cf "$package_path" .PKGINFO usr)
  printf '%s\n' "$package_path"
}

package_v1="$(build_package 1.17.0)"
package_v2="$(build_package 1.17.1)"

build_legacy_package() {
  local name="$1"
  local package_root="$work_root/legacy-$name"
  local package_path="$work_root/$name-1.16.0-1-x86_64.pkg.tar.zst"
  mkdir -p "$package_root/usr/bin" "$package_root/usr/share/applications"
  if [[ "$name" == "toolport-native-preview" ]]; then
    printf '#!/bin/sh\nexit 0\n' > "$package_root/usr/bin/toolport-gtk"
    chmod 755 "$package_root/usr/bin/toolport-gtk"
    printf '[Desktop Entry]\nType=Application\nName=Toolport Native Preview\nExec=toolport-gtk\n' \
      > "$package_root/usr/share/applications/com.tsout.Toolport.NativePreview.desktop"
  else
    printf '#!/bin/sh\nexit 0\n' > "$package_root/usr/bin/conduit"
    chmod 755 "$package_root/usr/bin/conduit"
  fi
  printf 'pkgname = %s\npkgbase = %s\npkgver = 1.16.0-1\npkgdesc = Legacy Toolport package\nurl = https://github.com/tsouth89/toolport\nbuilddate = 0\npackager = Toolport lifecycle smoke\nsize = 1\narch = x86_64\nlicense = MIT\n' \
    "$name" "$name" > "$package_root/.PKGINFO"
  (cd "$package_root" && bsdtar --zstd -cf "$package_path" .PKGINFO usr)
  printf '%s\n' "$package_path"
}

legacy_preview="$(build_legacy_package toolport-native-preview)"
legacy_tauri="$(build_legacy_package toolport-bin)"

pacman_root() {
  fakeroot pacman \
    --config "$work_root/pacman.conf" \
    --root "$root" \
    --dbpath "$database" \
    --cachedir "$cache" \
    --logfile "$work_root/pacman.log" \
    --nodeps \
    --noconfirm \
    "$@"
}

pacman_root_replace() {
  printf 'y\ny\n' | fakeroot pacman \
    --config "$work_root/pacman.conf" \
    --root "$root" \
    --dbpath "$database" \
    --cachedir "$cache" \
    --logfile "$work_root/pacman.log" \
    --nodeps \
    "$@"
}

assert_user_state_unchanged() {
  local after
  after="$(sha256sum \
    "$root/home/test/.config/Toolport/registry.json" \
    "$root/home/test/.config/Claude/claude_desktop_config.json")"
  [[ "$before" == "$after" ]]
}

pacman_root -U "$legacy_preview"
test -x "$root/usr/bin/toolport-gtk"
pacman_root_replace -U "$package_v1"
test ! -e "$root/usr/bin/toolport-gtk"
test ! -e "$root/usr/share/applications/com.tsout.Toolport.NativePreview.desktop"
test -x "$root/usr/bin/toolport"
test -x "$root/usr/bin/toolport-gateway"
plugin="$root/usr/share/toolport/agent-plugin/toolport-agent-plugin.zip"
test -f "$plugin"
bsdtar -tf "$plugin" | grep -q '^toolport/plugin.json$'
bsdtar -tf "$plugin" | grep -q '^toolport/skills/toolport/SKILL.md$'
assert_user_state_unchanged

pacman_root -U "$package_v2"
grep -q '^X-Toolport-Lifecycle=upgrade$' \
  "$root/usr/share/applications/app.toolport.Toolport.desktop"
assert_user_state_unchanged

pacman_root -U "$package_v1"
if grep -q '^X-Toolport-Lifecycle=upgrade$' \
  "$root/usr/share/applications/app.toolport.Toolport.desktop"; then
  echo "error: rollback retained the upgraded desktop payload" >&2
  exit 1
fi
assert_user_state_unchanged

pacman_root -R toolport
test ! -e "$root/usr/bin/toolport"
test ! -e "$root/usr/bin/toolport-gateway"
test ! -e "$root/usr/share/toolport/agent-plugin/toolport-agent-plugin.zip"
assert_user_state_unchanged

pacman_root -U "$legacy_tauri"
test -x "$root/usr/bin/conduit"
pacman_root_replace -U "$package_v1"
test ! -e "$root/usr/bin/conduit"
test -x "$root/usr/bin/toolport"
assert_user_state_unchanged

pacman_root -R toolport
assert_user_state_unchanged

echo "Linux-native package replacement, upgrade, rollback, and uninstall checks passed"
