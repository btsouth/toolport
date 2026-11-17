#!/usr/bin/env bash
# Agent verification loop for Toolport: build and serve on devbox, look from this
# workstation. Used by the verify skill (.claude/skills/verify/SKILL.md).
#
#   scripts/verify-devbox.sh sync            copy this worktree (uncommitted edits too) to devbox
#   scripts/verify-devbox.sh fixture         sync, start the browser fixture on devbox, tunnel it here
#   scripts/verify-devbox.sh fixture-stop    stop the fixture server and the tunnel
#   scripts/verify-devbox.sh gtk-build       sync, build toolport-gtk + gateway + mock on devbox, fetch them
#   scripts/verify-devbox.sh gtk-launch [--fresh|--empty]
#                                            start (or restart) the GTK app in this session's omabox on
#                                            isolated data: kept, --fresh reseeds one mock server,
#                                            --empty starts as a new user (setup dialog)
#   scripts/verify-devbox.sh headless        sync, run npm run verify:headless on devbox, fetch .verify
#   scripts/verify-devbox.sh parity [--only NAME]
#                                            sync, run the GTK vs React parity report on devbox (report
#                                            only), print it and fetch .verify/parity/run-*
#
# Environment: VERIFY_HOST (default devbox-bts), VERIFY_REMOTE_DIR (default
# /workspace/worktrees/toolport-verify-<worktree name>), VERIFY_PORT (fixture port).
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
NAME="$(basename "$ROOT")"
HOST="${VERIFY_HOST:-devbox-bts}"
REMOTE="${VERIFY_REMOTE_DIR:-/workspace/worktrees/toolport-verify-$NAME}"
OUT="$ROOT/.verify/desktop"
BIN="$OUT/bin"
FIXTURE_STATE="$OUT/fixture.env"
BOX_DATA=toolport-data # under the box HOME (/home/sbx)
FEATURES=gtk-desktop,search-static,test-support

say() { printf '%s\n' "$*" >&2; }
die() {
  say "verify-devbox: $*"
  exit 1
}
remote() { ssh -o BatchMode=yes "$HOST" "$@"; }

sync_tree() {
  local start=$SECONDS
  mkdir -p "$OUT"
  remote "mkdir -p '$REMOTE'"
  # Ignored paths (node_modules, target, .verify) are excluded, so --delete keeps the
  # devbox copies of them and only mirrors tracked and untracked source.
  rsync -az --delete --exclude=/.git --filter=':- .gitignore' "$ROOT/" "$HOST:$REMOTE/"
  say "synced to $HOST:$REMOTE ($((SECONDS - start))s)"
}

ensure_node_modules() {
  # Keyed on the lockfile's content: rsync keeps mtimes, so a time check can miss an edit.
  remote "cd '$REMOTE' && sum=\$(sha256sum package-lock.json) && if [ \"\$(cat node_modules/.verify-lock-sum 2>/dev/null)\" != \"\$sum\" ]; then heavy-run -- npm ci --no-audit --no-fund --loglevel=error && printf '%s' \"\$sum\" >node_modules/.verify-lock-sum; fi" >&2 # stdout carries only the fixture URL
}

port_free_here() { ! ss -Hltn "sport = :$1" | grep -q .; }

cmd_fixture() {
  [ -f "$FIXTURE_STATE" ] && cmd_fixture_stop quiet
  sync_tree
  ensure_node_modules
  local port="${VERIFY_PORT:-}"
  if [ -z "$port" ]; then
    for _ in $(seq 1 50); do
      port=$((20000 + RANDOM % 10000))
      port_free_here "$port" && remote "! ss -Hltn 'sport = :$port' | grep -q ." && break
      port=""
    done
    [ -n "$port" ] || die "no free port found"
  fi
  local log="$REMOTE.fixture.log"
  # Same server as npm run dev:fixture, without --open (no browser on devbox).
  local pid
  pid="$(remote "cd '$REMOTE' || exit 1; setsid nohup node_modules/.bin/vite --host 127.0.0.1 --port $port --strictPort >'$log' 2>&1 </dev/null & echo \$!")"
  nohup ssh -o BatchMode=yes -o ExitOnForwardFailure=yes -N -L "$port:127.0.0.1:$port" "$HOST" \
    >"$OUT/fixture-tunnel.log" 2>&1 </dev/null &
  local tunnel=$!
  printf 'PORT=%s\nREMOTE_PID=%s\nTUNNEL_PID=%s\n' "$port" "$pid" "$tunnel" >"$FIXTURE_STATE"
  local url="http://127.0.0.1:$port/fixtures/"
  mkdir -p "$ROOT/.verify/shots"
  for _ in $(seq 1 60); do
    # The answer must come from this run's vite and tunnel, not an older server on the port.
    if curl -fsS -o /dev/null "$url" 2>/dev/null && kill -0 "$tunnel" 2>/dev/null &&
      remote "kill -0 $pid" 2>/dev/null; then
      say "fixture ready (devbox log: $log)"
      printf '%s\n' "$url"
      return 0
    fi
    sleep 0.5
  done
  say "fixture did not answer at $url; devbox log tail:"
  remote "tail -20 '$log'" >&2 || true
  cmd_fixture_stop quiet
  exit 1
}

cmd_fixture_stop() {
  [ -f "$FIXTURE_STATE" ] || {
    [ "${1:-}" = quiet ] || say "no fixture running for this worktree"
    return 0
  }
  # shellcheck disable=SC1090
  . "$FIXTURE_STATE"
  kill "$TUNNEL_PID" 2>/dev/null || true
  remote "kill -TERM -- -$REMOTE_PID 2>/dev/null || kill $REMOTE_PID 2>/dev/null || true" ||
    say "could not reach $HOST to stop vite (pid $REMOTE_PID); stop it there by hand"
  rm -f "$FIXTURE_STATE"
  [ "${1:-}" = quiet ] || say "fixture stopped"
}

cmd_gtk_build() {
  sync_tree
  local start=$SECONDS
  # Own target dir per worktree keeps incremental builds; sccache shares dependencies.
  local log="$REMOTE.gtk-build.log"
  remote "cd '$REMOTE' && export PATH=\$HOME/.cargo/bin:\$PATH && heavy-run -- cargo build --locked \
    --manifest-path src-tauri/Cargo.toml --no-default-features --features $FEATURES \
    --bin toolport-gtk --bin toolport-gateway --bin mock-mcp-server >'$log' 2>&1; status=\$?; \
    grep -E '^(error|warning)|^ *--> |Finished' '$log' | tail -40; test \$status -eq 0 || exit 1; \
    mkdir -p .verify/desktop-bin && for b in toolport-gtk toolport-gateway mock-mcp-server; do \
    strip -o .verify/desktop-bin/\$b src-tauri/target/debug/\$b || exit 1; done" ||
    die "build failed on $HOST; full log: $log"
  mkdir -p "$BIN"
  rsync -az "$HOST:$REMOTE/.verify/desktop-bin/" "$BIN/"
  say "built and fetched to $BIN ($((SECONDS - start))s)"
}

seed_data() {
  local home="$1" dir="$1/$BOX_DATA"
  rm -rf "$dir"
  mkdir -p "$dir"
  cat >"$dir/registry.json" <<EOF
{
  "version": 3,
  "safetyLevel": "ask",
  "servers": [
    {
      "id": "mock",
      "name": "Mock",
      "transport": "stdio",
      "command": "$BIN/mock-mcp-server",
      "args": [],
      "env": [],
      "enabled": true
    }
  ],
  "profiles": [{ "id": "default", "name": "Default", "enabledServerIds": ["mock"] }],
  "activeProfileId": "default"
}
EOF
}

cmd_gtk_launch() {
  [ -x "$BIN/toolport-gtk" ] || die "no $BIN/toolport-gtk; run: scripts/verify-devbox.sh gtk-build"
  command -v omabox >/dev/null || die "omabox is not installed here"
  local mode="${1:-}"
  case "$mode" in "" | --fresh | --empty) ;; *) die "gtk-launch takes --fresh or --empty" ;; esac
  # --net isolated: the app must never reach the user's real gateway or services.
  if ! omabox path --home >/dev/null 2>&1; then
    (cd "$ROOT" && omabox up --net isolated) >&2
  fi
  local home
  home="$(cd "$ROOT" && omabox path --home)"
  local box net
  box="$(basename "$(dirname "$home")")"
  net="$(omabox ls --json | jq -r --arg b "$box" '.[] | select(.name == $b) | .net')"
  [ "$net" = isolated ] || die "box $box is up with --net $net; run omabox down, then gtk-launch again"
  mkdir -p "$ROOT/.verify/shots"
  # Stop any earlier copy first: a second one would hand over to it (single instance)
  # and the old binary would keep running.
  (cd "$ROOT" && omabox run -- sh -c 'pkill -x toolport-gtk; for _ in $(seq 50); do
    pgrep -x toolport-gtk >/dev/null || exit 0; sleep 0.1; done; pkill -9 -x toolport-gtk') >/dev/null 2>&1 || true
  if [ "$mode" = --empty ]; then
    rm -rf "${home:?}/$BOX_DATA" && mkdir -p "$home/$BOX_DATA" # a new user: setup runs
  elif [ "$mode" = --fresh ] || [ ! -d "$home/$BOX_DATA" ]; then
    seed_data "$home"
  fi
  (cd "$ROOT" && omabox run -d --wait --timeout 20s -- \
    env TOOLPORT_DATA_DIR="/home/sbx/$BOX_DATA" "$BIN/toolport-gtk") >&2
  (cd "$ROOT" && omabox windows) >&2
}

cmd_headless() {
  sync_tree
  ensure_node_modules
  local status=0
  remote "cd '$REMOTE' && export PATH=\$HOME/.cargo/bin:\$PATH && heavy-run -- npm run verify:headless" || status=$?
  rsync -az "$HOST:$REMOTE/.verify/" "$ROOT/.verify/" --include='run-*/***' --exclude='*'
  say "logs: $(ls -dt "$ROOT"/.verify/run-* 2>/dev/null | head -1)/summary.json"
  return "$status"
}

cmd_parity() {
  sync_tree
  ensure_node_modules
  local status=0
  # The GTK half renders under xvfb inside the script; nothing opens a window.
  remote "cd '$REMOTE' && export PATH=\$HOME/.cargo/bin:\$PATH && heavy-run -- npm run --silent parity -- $*" || status=$?
  mkdir -p "$ROOT/.verify/parity"
  local run
  run="$(remote "ls -dt '$REMOTE'/.verify/parity/run-* 2>/dev/null | head -1")"
  if [ -n "$run" ]; then
    rsync -az "$HOST:$run" "$ROOT/.verify/parity/"
    say "report: .verify/parity/$(basename "$run")/report.txt"
  fi
  return "$status"
}

case "${1:-}" in
  sync) sync_tree ;;
  fixture) cmd_fixture ;;
  fixture-stop) cmd_fixture_stop ;;
  gtk-build) cmd_gtk_build ;;
  gtk-launch) shift; cmd_gtk_launch "$@" ;;
  headless) cmd_headless ;;
  parity) shift; cmd_parity "$@" ;;
  *)
    sed -n '2,20p' "$0" | sed 's/^# \{0,1\}//'
    exit 2
    ;;
esac
