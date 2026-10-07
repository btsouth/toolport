#!/usr/bin/env bash
# Roll an Arch/Omarchy machine back from a Toolport 2.0 preview to the 1.x
# package. Works without the 2.0 binaries running.
#
#   toolport-preview-rollback.sh [--dry-run] [--package FILE]
#
# 1. Stops the Toolport app and gateways this user runs. Only processes whose
#    executable is an installed Toolport binary are signalled.
# 2. If the registry was migrated past v1, moves it aside (kept, never deleted)
#    and restores the newest registry.json.v1-<ms>.bak the migration wrote.
#    Refuses when no v1 backup exists.
# 3. Reinstalls 1.x: the newest cached toolport-1.* package, or the one given
#    with --package, through `sudo pacman -U`.
# 4. Prints what it changed.
set -euo pipefail

dry_run=0
package=""
while [ $# -gt 0 ]; do
  case "$1" in
    --dry-run) dry_run=1 ;;
    --package)
      [ $# -ge 2 ] || { echo "error: --package needs a file" >&2; exit 2; }
      package="$2"
      shift
      ;;
    -h | --help)
      sed -n '2,15p' "$0" | sed 's/^# \{0,1\}//'
      exit 0
      ;;
    *) echo "error: unknown argument: $1" >&2; exit 2 ;;
  esac
  shift
done

run() {
  if [ "$dry_run" = 1 ]; then
    printf 'would run:'
    printf ' %q' "$@"
    printf '\n'
  else
    "$@"
  fi
}

# Same resolution as the app: an explicit override, else the config dir, with
# the legacy Conduit leaf when only that exists.
data_dir="${TOOLPORT_DATA_DIR:-${CONDUIT_DATA_DIR:-}}"
if [ -z "$data_dir" ]; then
  config_base="${XDG_CONFIG_HOME:-$HOME/.config}"
  data_dir="$config_base/Toolport"
  if [ ! -e "$data_dir" ] && [ -e "$config_base/Conduit" ]; then
    data_dir="$config_base/Conduit"
  fi
fi
registry="$data_dir/registry.json"
bin_dir="${TOOLPORT_ROLLBACK_BIN_DIR:-/usr/bin}"
pkg_cache="${TOOLPORT_ROLLBACK_PKG_CACHE:-/var/cache/pacman/pkg}"

# A Toolport process of this user: its executable is one of the installed
# binaries, including one an upgrade already replaced on disk.
is_toolport_pid() {
  local exe
  exe="$(readlink "/proc/$1/exe" 2>/dev/null)" || return 1
  exe="${exe% (deleted)}"
  case "$exe" in
    "$bin_dir/toolport" | "$bin_dir/toolport-gtk" | "$bin_dir/toolport-gateway") return 0 ;;
    *) return 1 ;;
  esac
}

toolport_pids() {
  local proc pid
  for proc in /proc/[0-9]*; do
    pid="${proc#/proc/}"
    [ "$pid" = "$$" ] && continue
    [ -O "$proc" ] || continue
    is_toolport_pid "$pid" && echo "$pid"
  done
  return 0
}

echo "Data directory: $data_dir"

# 1. Stop the daemon by its advertised pid first, then anything else left.
stopped=0
for descriptor in "$data_dir"/daemon-*.json; do
  [ -f "$descriptor" ] || continue
  pid="$(sed -n 's/.*"pid"[[:space:]]*:[[:space:]]*\([0-9][0-9]*\).*/\1/p' "$descriptor" | head -n 1)"
  if [ -n "$pid" ] && [ -O "/proc/$pid" ] && is_toolport_pid "$pid"; then
    run kill -TERM "$pid"
    stopped=$((stopped + 1))
  fi
done
mapfile -t pids < <(toolport_pids)
for pid in "${pids[@]}"; do
  run kill -TERM "$pid" 2>/dev/null || true
  stopped=$((stopped + 1))
done
if [ "$dry_run" = 0 ] && [ "$stopped" -gt 0 ]; then
  for _ in $(seq 1 50); do
    [ -z "$(toolport_pids)" ] && break
    sleep 0.1
  done
  mapfile -t left < <(toolport_pids)
  for pid in "${left[@]}"; do
    kill -KILL "$pid" 2>/dev/null || true
  done
fi
echo "Stopped $stopped Toolport process(es)."

# 2. Put a v1 registry back if the preview migrated it.
# The top-level `version`; a registry from before versioning has none and is v1.
registry_version() {
  local version
  if command -v jq >/dev/null; then
    version="$(jq -r '.version // 1' "$1")"
  else
    version="$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1])).get("version", 1))' "$1")"
  fi
  case "$version" in
    '' | *[!0-9]*) echo "error: could not read the registry version from $1" >&2; exit 1 ;;
  esac
  echo "$version"
}

restored=""
if [ -f "$registry" ]; then
  version="$(registry_version "$registry")"
  if [ "$version" -gt 1 ]; then
    backup="$(find "$data_dir" -maxdepth 1 -name 'registry.json.v1-*.bak' -printf '%f\n' | sort | tail -n 1)"
    if [ -z "$backup" ]; then
      echo "error: the registry is at version $version and no registry.json.v1-*.bak exists in $data_dir." >&2
      echo "Toolport 1.x cannot open it. Nothing was changed; reinstall the preview or restore a backup by hand." >&2
      exit 1
    fi
    aside="$registry.v$version-rollback-$(date +%Y%m%d-%H%M%S)"
    run mv "$registry" "$aside"
    run cp -p "$data_dir/$backup" "$registry"
    restored="$backup"
    stamp="${backup#registry.json.v1-}"
    stamp="${stamp%.bak}"
    when="$(date -d "@$((stamp / 1000))" '+%Y-%m-%d %H:%M:%S' 2>/dev/null || echo "$stamp")"
    echo "Registry: version $version moved to $(basename "$aside"); restored $backup (taken $when)."
    echo "Changes made in the preview after that time are not in the restored registry."
  else
    echo "Registry: version $version, which 1.x reads as is. Left in place."
  fi
else
  echo "Registry: none at $registry. Nothing to restore."
fi

# 3. Reinstall 1.x.
if [ -z "$package" ]; then
  package="$(find "$pkg_cache" -maxdepth 1 -name 'toolport-1.*-x86_64.pkg.tar.zst' -printf '%f\n' 2>/dev/null | sort -V | tail -n 1)"
  [ -n "$package" ] && package="$pkg_cache/$package"
fi
if [ -n "$package" ]; then
  [ -f "$package" ] || { echo "error: no package at $package" >&2; exit 1; }
  echo "Reinstalling $(basename "$package")."
  run sudo pacman -U "$package"
else
  echo "No cached toolport-1.* package in $pkg_cache; installing from the repo."
  run sudo pacman -S toolport
fi

echo
echo "Done. Start Toolport again from the launcher; clients reconnect to the 1.x gateway on their next start."
[ -n "$restored" ] && echo "The preview registry was kept beside the restored one."
exit 0
