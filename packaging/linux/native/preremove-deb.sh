#!/bin/sh
# deb: all other arguments describe upgrade, rollback or failed transactions.
[ "${1:-}" = remove ] || exit 0
[ "${2:-}" != in-favour ] || exit 0
if [ -x /usr/share/toolport/disconnect-users.sh ]; then
  /usr/share/toolport/disconnect-users.sh || true
else
  echo 'Toolport: cleanup helper missing. Reinstall and run toolport-gateway --disconnect-all as your user before deleting Toolport data. Removal will continue.' >&2
fi
exit 0
