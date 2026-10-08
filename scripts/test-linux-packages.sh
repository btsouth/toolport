#!/usr/bin/env bash
# Install real packages in disposable containers without a desktop or keyring.
set -euo pipefail

smoke() {
  local expected_version=$1
  test "$(readlink -f /usr/bin/toolport)" = /usr/bin/toolport-gtk
  test "$(readlink -f /usr/bin/conduit)" = /usr/bin/toolport-gtk
  cmp /usr/bin/toolport-gtk /packages/toolport-gtk
  test -x /usr/bin/toolport-gateway
  for binary in /usr/bin/toolport-gtk /usr/bin/toolport-gateway /usr/share/toolport/toolport-preview-rollback.sh; do
    test "$(stat -c '%a' "$binary")" = 755
  done
  test -f /usr/share/applications/com.tsout.Toolport.desktop
  test "$(stat -c '%a' /usr/share/applications/com.tsout.Toolport.desktop)" = 644
  grep -qx 'Exec=toolport-gtk %U' /usr/share/applications/com.tsout.Toolport.desktop
  grep -qx 'Icon=toolport' /usr/share/applications/com.tsout.Toolport.desktop
  grep -qx 'Categories=Development;' /usr/share/applications/com.tsout.Toolport.desktop
  test -f /usr/share/metainfo/com.tsout.Toolport.metainfo.xml
  for size in 32x32 128x128 256x256; do
    test -f "/usr/share/icons/hicolor/$size/apps/toolport.png"
  done
  test -f /usr/share/toolport/agent-plugin/toolport-agent-plugin.zip
  test -x /usr/share/toolport/toolport-preview-rollback.sh
  test -f /usr/share/licenses/toolport/LICENSE
  ldd /usr/bin/toolport-gtk | tee /tmp/gtk-ldd.txt
  if grep -q 'not found' /tmp/gtk-ldd.txt; then exit 1; fi
  grep -q 'libgtk-4.so' /tmp/gtk-ldd.txt
  grep -q 'libadwaita-1.so' /tmp/gtk-ldd.txt
  if grep -q 'libwebkit' /tmp/gtk-ldd.txt; then exit 1; fi
  ldd /usr/bin/toolport-gateway | tee /tmp/gateway-ldd.txt
  if grep -q 'not found' /tmp/gateway-ldd.txt; then exit 1; fi
  test "$(toolport-gateway --version)" = "toolport-gateway $expected_version"
  echo "PASS: GTK binary matches build, package contents and all libraries resolve"
  echo "PASS: $(toolport-gateway --version)"
}

if [ "${1:-}" = --container ]; then
  mode=${2:?}
  expected_version=${3:?}
  if [ "$mode" = fedora ]; then
    # The Fedora base image omits cmp; this is a test helper, not an app dependency.
    dnf install -y diffutils python3
    dnf install -y /packages/new.rpm
    rpm -qlp /packages/new.rpm
    test "$(rpm -qf --qf '%{NAME}' /usr/bin/toolport-gtk)" = toolport
  else
    export DEBIAN_FRONTEND=noninteractive
    apt-get update
    apt-get install -y python3
    if [ "$mode" = ubuntu ]; then
      test "$(dpkg-deb -f /packages/old.deb Package)" = toolport
      test "$(dpkg-deb -f /packages/old.deb Version)" = 1.24.0
      # Both packages deliberately have the same name. dpkg removes obsolete
      # owned files itself; no maintainer script should touch user data.
      apt install -y /packages/old.deb
      test -x /usr/bin/conduit
      test ! -e /usr/bin/toolport-gtk
      old_version=$(dpkg-query -W -f='${Version}' toolport)
      new_version=$(dpkg-deb -f /packages/new.deb Version)
      dpkg --compare-versions "$new_version" gt "$old_version"
      useradd -m toolport-test
      data=/home/toolport-test/.config/Toolport
      mkdir -p "$data" /home/toolport-test/.config/Claude /home/toolport-test/.local/share/Toolport
      cat > "$data/registry.json" <<'REGISTRY'
{"version":1,"servers":[{"id":"upgrade-fixture","name":"Preserved server","transport":"stdio","command":"printf","args":["fixture"],"env":[]}],"profiles":[{"id":"default","name":"Default","serverIds":["upgrade-fixture"]}]}
REGISTRY
      printf 'opaque encrypted credential fixture\n' > "$data/secrets.enc"
      printf 'opaque local encryption key fixture\n' > "$data/secrets.key"
      printf '{"mcpServers":{"toolport":{"command":"/usr/bin/toolport-gateway"}}}\n' \
        > /home/toolport-test/.config/Claude/claude_desktop_config.json
      printf 'preserved shared state\n' > /home/toolport-test/.local/share/Toolport/fixture
      chmod 600 "$data/secrets.enc" "$data/secrets.key"
      chown -R toolport-test:toolport-test /home/toolport-test
      (cd /home/toolport-test && find . -type f -print0 | sort -z | xargs -0 sha256sum) > /tmp/data-before.sha256
      (cd /home/toolport-test && find . -printf '%p %m %U %G\n' | sort) > /tmp/metadata-before.txt
    fi
    apt install -y /packages/new.deb
    dpkg-deb -c /packages/new.deb
    test "$(dpkg-query -W -f='${Version}' toolport)" = "$(dpkg-deb -f /packages/new.deb Version)"
    dpkg-query --search /usr/bin/toolport-gtk | grep -Ex 'toolport(:amd64)?: /usr/bin/toolport-gtk'
    test -s /usr/share/doc/toolport/copyright
    dpkg-deb -f /packages/new.deb Description | grep -q 'Manage MCP servers'
    if [ "$mode" = ubuntu ]; then
      (cd /home/toolport-test && sha256sum -c /tmp/data-before.sha256)
      (cd /home/toolport-test && find . -printf '%p %m %U %G\n' | sort) > /tmp/metadata-after.txt
      cmp /tmp/metadata-before.txt /tmp/metadata-after.txt
      test ! -e /usr/share/applications/Toolport.desktop
      test -z "$(find /usr/share/icons/hicolor -name conduit.png -print -quit)"
      test "$(runuser -u toolport-test -- toolport-gateway --version)" = "toolport-gateway $expected_version"
      echo "PASS: Ubuntu 24.04 apt upgrade $old_version -> $new_version; registry, server, clients and credential fixture bytes/modes/owners unchanged"
      echo "PASS: legacy conduit/toolport paths point to GTK; old desktop entry and icons removed"
    fi
  fi
  smoke "$expected_version"
  bash /packages/roundtrip.sh "$mode"
  echo "PASS: $mode install"
  exit 0
fi

cd "$(dirname "$0")/.."
deb=${1:?usage: test-linux-packages.sh NEW.deb NEW.rpm OLD.deb [ubuntu|debian|fedora]}
rpm=${2:?}
old_deb=${3:?}
selected=${4:-all}
case "$selected" in
  all|ubuntu|debian|fedora) ;;
  *) echo "error: unknown test distribution: $selected" >&2; exit 2 ;;
esac
version=$(sed -n 's/^version = "\([^"]*\)"/\1/p' src-tauri/Cargo.toml | head -1)
for path in "$deb" "$rpm" "$old_deb" src-tauri/target/release/toolport-gtk; do
  test -f "$path"
done
tmp=$(mktemp -d)
# Names are task-owned and unique so cleanup cannot affect parallel work.
prefix=toolport-p4-pkg-$$
cleanup() {
  for mode in ubuntu debian fedora; do
    docker rm -f "$prefix-$mode" >/dev/null 2>&1 || true
    docker image rm "$prefix-$mode:latest" >/dev/null 2>&1 || true
  done
  rm -rf "$tmp"
}
trap cleanup EXIT
cp "$deb" "$tmp/new.deb"
cp "$rpm" "$tmp/new.rpm"
cp "$old_deb" "$tmp/old.deb"
cp src-tauri/target/release/toolport-gtk "$tmp/toolport-gtk"
cp scripts/test-linux-packages.sh "$tmp/test.sh"
cp scripts/test-package-removal.sh "$tmp/roundtrip.sh"
cp scripts/package-removal-fixture.py "$tmp/fixture.py"
chmod 755 "$tmp"
for mode in ubuntu debian fedora; do
  if [ "$selected" != all ] && [ "$selected" != "$mode" ]; then continue; fi
  case "$mode" in
    ubuntu) image=ubuntu:24.04 ;;
    debian) image=debian:13 ;;
    fedora) image=fedora:latest ;;
  esac
  # Tag only task-owned aliases. Keep shared upstream caches and containers.
  timeout 300 docker pull "$image"
  alias_image="$prefix-$mode:latest"
  docker tag "$image" "$alias_image"
  if timeout 900 docker run --name "$prefix-$mode" --network "${TOOLPORT_PACKAGE_TEST_NETWORK:-bridge}" \
    -v "$tmp:/packages:ro" "$alias_image" bash /packages/test.sh --container "$mode" "$version"; then
    docker rm "$prefix-$mode" >/dev/null
    docker image rm "$alias_image" >/dev/null
  else
    docker image rm "$alias_image" >/dev/null 2>&1 || true
    exit 1
  fi
done
