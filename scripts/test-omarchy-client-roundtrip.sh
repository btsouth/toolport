#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
host_home="$HOME"
cargo_home="${CARGO_HOME:-$host_home/.cargo}"
rustup_home="${RUSTUP_HOME:-$host_home/.rustup}"
test_root="$(mktemp -d /tmp/toolport-omarchy-clients.XXXXXX)"
trap 'rm -rf "$test_root"' EXIT
test_home="$test_root/home"
mkdir -p "$test_home"

HOME="$test_home" \
CARGO_HOME="$cargo_home" \
RUSTUP_HOME="$rustup_home" \
XDG_CONFIG_HOME="$test_home/.config" \
XDG_DATA_HOME="$test_home/.local/share" \
CODEX_HOME="$test_home/.codex" \
COPILOT_HOME="$test_home/.copilot" \
GROK_HOME="$test_home/.grok" \
GEMINI_CLI_HOME="$test_home" \
CLAUDE_CONFIG_DIR="$test_home/.claude-profile" \
TOOLPORT_OMARCHY_CLIENT_TEST_ROOT="$test_root" \
  cargo test \
    --manifest-path "$repo_root/src-tauri/Cargo.toml" \
    --no-default-features \
    --lib \
    clients::tests::omarchy_clients_round_trip_without_losing_existing_config \
    -- --ignored --exact --nocapture --test-threads=1

echo "Omarchy client connect, rollback, and disconnect contract passed for 10 clients"
