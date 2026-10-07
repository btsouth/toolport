#!/usr/bin/env bash
# Tests for toolport-preview-rollback.sh. Everything runs against a scratch data
# dir, a scratch package cache and stub sudo/pacman; nothing touches the real
# system or a real Toolport install.
# `pass` cannot fail, so `check && pass || fail` is a safe if-then-else here.
# shellcheck disable=SC2015
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
script="$repo_root/scripts/toolport-preview-rollback.sh"
work="$(mktemp -d /tmp/toolport-rollback-tests.XXXXXX)"
pids=()
cleanup() {
  for pid in "${pids[@]}"; do kill -KILL "$pid" 2>/dev/null || true; done
  rm -rf "$work"
}
trap cleanup EXIT

failures=0
fail() { echo "FAIL: $*" >&2; failures=$((failures + 1)); }
pass() { echo "ok: $*"; }

# Stub sudo and pacman that only record their arguments.
stubs="$work/stubs"
mkdir -p "$stubs"
cat > "$stubs/sudo" <<'EOF'
#!/usr/bin/env bash
"$@"
EOF
cat > "$stubs/pacman" <<'EOF'
#!/usr/bin/env bash
echo "$*" >> "$PACMAN_LOG"
EOF
chmod +x "$stubs/sudo" "$stubs/pacman"

# One fresh case: data dir, bin dir, cache, pacman log.
setup() {
  case_dir="$work/$1"
  mkdir -p "$case_dir/data" "$case_dir/bin" "$case_dir/cache"
  export TOOLPORT_DATA_DIR="$case_dir/data"
  export TOOLPORT_ROLLBACK_BIN_DIR="$case_dir/bin"
  export TOOLPORT_ROLLBACK_PKG_CACHE="$case_dir/cache"
  export PACMAN_LOG="$case_dir/pacman.log"
  : > "$PACMAN_LOG"
}

rollback() {
  PATH="$stubs:$PATH" "$script" "$@"
}

# v1 registry: left alone; the newest cached 1.x package is reinstalled.
setup v1
printf '{\n  "servers": [{"id": "a", "version": 7}],\n  "version": 1\n}\n' > "$TOOLPORT_DATA_DIR/registry.json"
touch "$TOOLPORT_ROLLBACK_PKG_CACHE/toolport-1.23.5-1-x86_64.pkg.tar.zst" \
  "$TOOLPORT_ROLLBACK_PKG_CACHE/toolport-1.24.0-1-x86_64.pkg.tar.zst" \
  "$TOOLPORT_ROLLBACK_PKG_CACHE/toolport-2.0.0preview.1-1-x86_64.pkg.tar.zst"
out="$(rollback)"
grep -q '"id": "a"' "$TOOLPORT_DATA_DIR/registry.json" && pass "v1 registry left in place" || fail "v1 registry changed"
grep -qx -- "-U $TOOLPORT_ROLLBACK_PKG_CACHE/toolport-1.24.0-1-x86_64.pkg.tar.zst" "$PACMAN_LOG" \
  && pass "newest cached 1.x package reinstalled" || fail "pacman got: $(cat "$PACMAN_LOG")"
echo "$out" | grep -q "reads as is" && pass "v1 reported as kept" || fail "output: $out"

# v2 registry with two v1 backups: moved aside, newest backup restored.
setup v2
printf '{"version": 2, "servers": [{"id": "new"}]}\n' > "$TOOLPORT_DATA_DIR/registry.json"
printf '{"version": 1, "servers": [{"id": "old"}]}\n' > "$TOOLPORT_DATA_DIR/registry.json.v1-1790000000000.bak"
printf '{"version": 1, "servers": [{"id": "newest-v1"}]}\n' > "$TOOLPORT_DATA_DIR/registry.json.v1-1791000000000.bak"
touch "$TOOLPORT_ROLLBACK_PKG_CACHE/toolport-1.24.0-1-x86_64.pkg.tar.zst"
out="$(rollback)"
grep -q newest-v1 "$TOOLPORT_DATA_DIR/registry.json" && pass "newest v1 backup restored" || fail "restored: $(cat "$TOOLPORT_DATA_DIR/registry.json")"
aside="$(find "$TOOLPORT_DATA_DIR" -name 'registry.json.v2-rollback-*')"
[ -n "$aside" ] && grep -q '"new"' "$aside" && pass "v2 registry kept aside" || fail "no v2 copy kept"
[ -f "$TOOLPORT_DATA_DIR/registry.json.v1-1791000000000.bak" ] && pass "backup itself kept" || fail "backup consumed"
echo "$out" | grep -q "restored registry.json.v1-1791000000000.bak" && pass "restore reported" || fail "output: $out"

# A registry the real v1 -> v2 migration wrote (the Rust test
# rollback_fixture_is_a_real_migration keeps v2.json equal to its output for
# v1.json): the exact v1 file comes back and the exports stay.
setup v2-migrated
fixtures="$repo_root/src-tauri/tests/fixtures/registry-v1-to-v2"
cp "$fixtures/v2.json" "$TOOLPORT_DATA_DIR/registry.json"
cp "$fixtures/v1.json" "$TOOLPORT_DATA_DIR/registry.json.v1-1791000000000.bak"
mkdir -p "$TOOLPORT_DATA_DIR/exports"
printf 'Always run the tests.\n' > "$TOOLPORT_DATA_DIR/exports/rules-2026-10-07.md"
touch "$TOOLPORT_ROLLBACK_PKG_CACHE/toolport-1.24.0-1-x86_64.pkg.tar.zst"
out="$(rollback)"
cmp -s "$fixtures/v1.json" "$TOOLPORT_DATA_DIR/registry.json" && pass "migrated registry rolled back to the exact v1 file" || fail "restored: $(head -c 200 "$TOOLPORT_DATA_DIR/registry.json")"
aside="$(find "$TOOLPORT_DATA_DIR" -name 'registry.json.v2-rollback-*')"
[ -n "$aside" ] && cmp -s "$fixtures/v2.json" "$aside" && pass "migrated registry kept aside" || fail "no exact v2 copy kept"
[ -f "$TOOLPORT_DATA_DIR/exports/rules-2026-10-07.md" ] && pass "exports left in place" || fail "exports removed"
echo "$out" | grep -q "Registry: version 2 moved to" && pass "migrated version reported" || fail "output: $out"

# v2 registry with no v1 backup: refuses and changes nothing.
setup v2-no-backup
printf '{"version": 2}\n' > "$TOOLPORT_DATA_DIR/registry.json"
touch "$TOOLPORT_ROLLBACK_PKG_CACHE/toolport-1.24.0-1-x86_64.pkg.tar.zst"
if rollback >/dev/null 2>"$case_dir/err"; then
  fail "rollback without a v1 backup succeeded"
else
  pass "refuses without a v1 backup"
fi
grep -q '"version": 2' "$TOOLPORT_DATA_DIR/registry.json" && pass "registry untouched on refusal" || fail "registry changed on refusal"
[ ! -s "$PACMAN_LOG" ] && pass "nothing reinstalled on refusal" || fail "pacman ran: $(cat "$PACMAN_LOG")"

# No cached package: falls back to the repo.
setup no-cache
printf '{"version": 1}\n' > "$TOOLPORT_DATA_DIR/registry.json"
rollback >/dev/null
grep -qx -- "-S toolport" "$PACMAN_LOG" && pass "falls back to the repo" || fail "pacman got: $(cat "$PACMAN_LOG")"

# Dry run: reports, changes nothing.
setup dry-run
printf '{"version": 2}\n' > "$TOOLPORT_DATA_DIR/registry.json"
printf '{"version": 1}\n' > "$TOOLPORT_DATA_DIR/registry.json.v1-1791000000000.bak"
out="$(rollback --dry-run)"
grep -q '"version": 2' "$TOOLPORT_DATA_DIR/registry.json" && pass "dry run leaves the registry" || fail "dry run changed the registry"
[ ! -s "$PACMAN_LOG" ] && pass "dry run installs nothing" || fail "dry run ran pacman"
echo "$out" | grep -q "would run: sudo pacman -S toolport" && pass "dry run prints the install" || fail "output: $out"

# Processes: a Toolport binary (by executable path) stops; an unrelated
# process with the same name elsewhere does not.
setup processes
printf '{"version": 1}\n' > "$TOOLPORT_DATA_DIR/registry.json"
sleep_bin="$(command -v sleep)"
cp "$sleep_bin" "$TOOLPORT_ROLLBACK_BIN_DIR/toolport-gateway"
mkdir -p "$case_dir/elsewhere"
cp "$sleep_bin" "$case_dir/elsewhere/toolport-gateway"
"$TOOLPORT_ROLLBACK_BIN_DIR/toolport-gateway" 300 &
daemon=$!
pids+=("$daemon")
"$case_dir/elsewhere/toolport-gateway" 300 &
stranger=$!
pids+=("$stranger")
printf '{"endpoint":"127.0.0.1:1","pid":%s}\n' "$daemon" > "$TOOLPORT_DATA_DIR/daemon-test.json"
sleep 0.2
rollback >/dev/null
sleep 0.2
if kill -0 "$daemon" 2>/dev/null; then fail "Toolport gateway still running"; else pass "Toolport gateway stopped"; fi
if kill -0 "$stranger" 2>/dev/null; then pass "unrelated process left alone"; else fail "unrelated process was killed"; fi

if [ "$failures" -gt 0 ]; then
  echo "$failures failure(s)" >&2
  exit 1
fi
echo "all rollback tests passed"
