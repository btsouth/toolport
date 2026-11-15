#!/usr/bin/env bash
set -euo pipefail
root=$(cd "$(dirname "$0")/.." && pwd)
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
mkdir -p "$tmp/bin" "$tmp/data"
cat > "$tmp/bin/toolport-gateway" <<'FAKE'
#!/bin/sh
printf '%s\n' "$*"
FAKE
chmod +x "$tmp/bin/toolport-gateway"
export PATH="$tmp/bin:$PATH" TOOLPORT_REGISTRY="$tmp/data/registry.json"
unset TOOLPORT_HTTP_PORT CONDUIT_HTTP_PORT
check() {
  local expected=$1; shift
  local actual
  actual=$("$@")
  test "$actual" = "$expected" || { printf 'FAIL: wanted %s, got %s\n' "$expected" "$actual"; exit 1; }
}
check '--http 8765' sh "$root/docker-entrypoint.sh" toolport-gateway --http 8765
check '--http 18765 --verbose' env TOOLPORT_HTTP_PORT=18765 sh "$root/docker-entrypoint.sh" toolport-gateway --http 8765 --verbose
check '--http 18999' env CONDUIT_HTTP_PORT=18999 sh "$root/docker-entrypoint.sh" toolport-gateway --http 8765
check '--http 9000' env TOOLPORT_HTTP_PORT=18765 sh "$root/docker-entrypoint.sh" toolport-gateway --http 9000
for value in invalid 65536 99999999999999999999999; do
  if env TOOLPORT_HTTP_PORT="$value" sh "$root/docker-entrypoint.sh" toolport-gateway --http 8765 >"$tmp/out" 2>&1; then
    echo "FAIL: accepted invalid port $value"; exit 1
  fi
  grep -q 'TOOLPORT_HTTP_PORT must be' "$tmp/out"
done
echo 'PASS: Docker default, configured/legacy/explicit ports, extra arguments, invalid ports'
