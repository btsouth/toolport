#!/usr/bin/env bash
# Called inside the disposable package container after its installation smoke.
set -euo pipefail
mode=${1:?}
for user in toolport-remove-a toolport-remove-b toolport-remove-empty; do
  useradd -m "$user"
done
python3 /packages/fixture.py
for user in toolport-remove-a toolport-remove-b; do
  chown -R "$user:$user" "/home/$user"
done
# Replacing an installed package exercises the real upgrade argument convention.
if [ "$mode" = arch ]; then
  pacman -U --noconfirm /packages/upgrade.pkg.tar.zst
elif [ "$mode" = fedora ]; then
  rpm -U --replacepkgs /packages/new.rpm
else
  dpkg -i /packages/new.deb
fi
for user in toolport-remove-a toolport-remove-b; do
  cmp "/tmp/$user-connected" "/home/$user/.cursor/mcp.json"
done
echo "PASS: $mode upgrade leaves connected client bytes unchanged"
if [ "$mode" = arch ]; then
  pacman -R --noconfirm toolport
elif [ "$mode" = fedora ]; then
  dnf remove -y toolport
else
  dpkg --purge toolport
fi
for user in toolport-remove-a toolport-remove-b; do
  cmp "/tmp/$user-original" "/home/$user/.cursor/mcp.json"
  test "$(stat -c %U "/home/$user/.cursor/mcp.json")" = "$user"
done
test ! -e /home/toolport-remove-empty/.config/Toolport
test ! -e /usr/bin/toolport-gateway
echo "PASS: $mode removal restores exact original bytes for two users; ownership retained, account without data untouched"
