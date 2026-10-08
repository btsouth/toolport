#!/bin/sh
# Package-owned helper. Never inherit root's config, data override or shell.
PATH=/usr/sbin:/usr/bin:/sbin:/bin
export PATH
manual() {
  echo "Toolport: $*. Removal will continue. Before deleting Toolport data, reinstall Toolport and run toolport-gateway --disconnect-all as the affected user, or use Settings > Remove Toolport from all clients." >&2
}
if [ ! -x /usr/bin/toolport-gateway ]; then
  manual "gateway binary is missing; client configs were not restored"
  exit 0
fi
accounts=$(getent passwd) || {
  manual "could not enumerate user accounts; client configs were not restored"
  exit 0
}
printf '%s\n' "$accounts" | while IFS=: read -r user _password uid _gid _gecos home _shell; do
  case "$uid" in ''|*[!0-9]*|0) continue ;; esac
  case "$home" in /*) ;; *) continue ;; esac
  # Standard current and pre-rename data locations only. No uid floor: system
  # accounts are eligible only when they actually have Toolport state.
  if [ ! -d "$home/.config/Toolport" ] && [ ! -d "$home/.config/Conduit" ]; then
    continue
  fi
  echo "Toolport: restoring client configs for $user"
  if command -v runuser >/dev/null 2>&1; then
    runuser -u "$user" -- env -i HOME="$home" USER="$user" LOGNAME="$user" \
      PATH=/usr/bin:/bin XDG_CONFIG_HOME="$home/.config" \
      timeout 30 /usr/bin/toolport-gateway --disconnect-all || manual "cleanup failed for $user"
  elif command -v su >/dev/null 2>&1; then
    # shellcheck disable=SC2016 # Positional args expand in the user shell.
    su -s /bin/sh -c 'exec env -i HOME="$1" USER="$2" LOGNAME="$2" PATH=/usr/bin:/bin XDG_CONFIG_HOME="$1/.config" timeout 30 /usr/bin/toolport-gateway --disconnect-all' \
      -- "$user" sh "$home" "$user" || manual "cleanup failed for $user"
  else
    manual "runuser and su are unavailable; cleanup skipped for $user"
  fi
done
exit 0
