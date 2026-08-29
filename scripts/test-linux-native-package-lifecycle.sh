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
  printf 'pkgname = toolport-native-preview\npkgbase = toolport-native-preview\npkgver = %s-1\npkgdesc = Native GTK Toolport shell and local MCP gateway\nurl = https://github.com/tsouth89/toolport\nbuilddate = 0\npackager = Toolport lifecycle smoke\nsize = 1\narch = x86_64\nlicense = MIT\n' \
    "$version" > "$package_root/.PKGINFO"
  if [[ "$version" == "1.17.1" ]]; then
    printf 'X-Toolport-Lifecycle=upgrade\n' \
      >> "$package_root/usr/share/applications/com.tsout.Toolport.NativePreview.desktop"
  fi
  (cd "$package_root" && bsdtar --zstd -cf "$package_path" .PKGINFO usr)
  printf '%s\n' "$package_path"
}

package_v1="$(build_package 1.17.0)"
package_v2="$(build_package 1.17.1)"

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

assert_user_state_unchanged() {
  local after
  after="$(sha256sum \
    "$root/home/test/.config/Toolport/registry.json" \
    "$root/home/test/.config/Claude/claude_desktop_config.json")"
  [[ "$before" == "$after" ]]
}

pacman_root -U "$package_v1"
test -x "$root/usr/bin/toolport-gtk"
test -x "$root/usr/bin/toolport-gateway"
plugin="$root/usr/share/toolport/agent-plugin/toolport-agent-plugin.zip"
test -f "$plugin"
bsdtar -tf "$plugin" | grep -q '^toolport/plugin.json$'
bsdtar -tf "$plugin" | grep -q '^toolport/skills/toolport/SKILL.md$'
assert_user_state_unchanged

pacman_root -U "$package_v2"
grep -q '^X-Toolport-Lifecycle=upgrade$' \
  "$root/usr/share/applications/com.tsout.Toolport.NativePreview.desktop"
assert_user_state_unchanged

pacman_root -U "$package_v1"
if grep -q '^X-Toolport-Lifecycle=upgrade$' \
  "$root/usr/share/applications/com.tsout.Toolport.NativePreview.desktop"; then
  echo "error: rollback retained the upgraded desktop payload" >&2
  exit 1
fi
assert_user_state_unchanged

pacman_root -R toolport-native-preview
test ! -e "$root/usr/bin/toolport-gtk"
test ! -e "$root/usr/bin/toolport-gateway"
test ! -e "$root/usr/share/toolport/agent-plugin/toolport-agent-plugin.zip"
assert_user_state_unchanged

echo "Linux-native package install, upgrade, rollback, and uninstall checks passed"
