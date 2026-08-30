#!/usr/bin/env bash
set -euo pipefail

launcher="${1:-/usr/share/omarchy/bin/omarchy-agent}"
flavor="${2:-auto}"

if [[ ! -x "$launcher" ]]; then
  echo "error: Omarchy agent launcher is not executable: $launcher" >&2
  exit 1
fi

if [[ "$flavor" == "auto" ]]; then
  if rg -q '^ori\)' "$launcher"; then
    flavor=edge
  elif rg -q '^gemini\)' "$launcher"; then
    flavor=stable
  else
    echo "error: could not identify the launcher as stable or edge" >&2
    exit 1
  fi
fi

if [[ "$flavor" != "stable" && "$flavor" != "edge" ]]; then
  echo "error: flavor must be stable, edge, or auto" >&2
  exit 1
fi

test_root="$(mktemp -d /tmp/toolport-omarchy-launcher.XXXXXX)"
trap 'rm -rf "$test_root"' EXIT
shim_dir="$test_root/bin"
test_home="$test_root/home"
trace="$test_root/trace"
stdout="$test_root/stdout"
stderr="$test_root/stderr"
mkdir -p "$shim_dir" "$test_home/Work" "$test_root/project"

printf '%s\n' \
  '#!/usr/bin/env bash' \
  '{' \
  '  printf "%s" "$(basename "$0")"' \
  '  if (($#)); then printf " <%s>" "$@"; fi' \
  '  printf "\n"' \
  '} > "$TOOLPORT_OMARCHY_TRACE"' \
  > "$shim_dir/record"

printf '%s\n' \
  '#!/usr/bin/env bash' \
  'printf "%s\n" "${TOOLPORT_TEST_SELECTOR:-}"' \
  > "$shim_dir/omarchy-default-agent"

printf '%s\n' \
  '#!/usr/bin/env bash' \
  '[[ ${TOOLPORT_TEST_MISSING:-0} == 1 ]]' \
  > "$shim_dir/omarchy-cmd-missing"

chmod 755 \
  "$shim_dir/record" \
  "$shim_dir/omarchy-default-agent" \
  "$shim_dir/omarchy-cmd-missing"

for command in \
  agy claude codex copilot crush gemini grok omp opencode ori pi \
  omarchy-launch-tui; do
  ln -s record "$shim_dir/$command"
done

run_launcher() {
  local selector="$1"
  shift
  rm -f "$trace" "$stdout" "$stderr"
  (
    cd "$test_root/project"
    HOME="$test_home" \
      PATH="$shim_dir:$PATH" \
      TOOLPORT_OMARCHY_TRACE="$trace" \
      TOOLPORT_TEST_SELECTOR="$selector" \
      "$launcher" "$@" > "$stdout" 2> "$stderr"
  )
}

expect_trace() {
  local selector="$1"
  local expected="$2"
  shift 2
  run_launcher "$selector" "$@"
  local actual
  actual="$(<"$trace")"
  if [[ "$actual" != "$expected" ]]; then
    echo "error: $selector produced an unexpected launch" >&2
    echo "expected: $expected" >&2
    echo "actual:   $actual" >&2
    exit 1
  fi
}

expect_failure() {
  local expected="$1"
  shift
  rm -f "$trace" "$stdout" "$stderr"
  if (
    cd "$test_root/project"
    HOME="$test_home" \
      PATH="$shim_dir:$PATH" \
      TOOLPORT_OMARCHY_TRACE="$trace" \
      "$@" > "$stdout" 2> "$stderr"
  ); then
    echo "error: launcher unexpectedly succeeded" >&2
    exit 1
  fi
  if ! rg -q --fixed-strings "$expected" "$stderr"; then
    echo "error: launcher failure did not contain: $expected" >&2
    sed -n '1,20p' "$stderr" >&2
    exit 1
  fi
}

prompt=release-fixture

expect_trace opencode 'opencode <--auto>' --inline
expect_trace opencode "opencode <--auto> <--prompt> <$prompt>" --inline --prompt "$prompt"
expect_trace copilot 'copilot <--allow-all>' --inline
expect_trace copilot "copilot <--allow-all> <--interactive> <$prompt>" --inline --prompt "$prompt"
expect_trace crush 'crush <--yolo>' --inline
expect_trace crush "crush <run> <$prompt>" --inline --prompt "$prompt"
expect_trace claude 'claude <--permission-mode> <auto>' --inline
expect_trace claude "claude <--permission-mode> <auto> <--> <$prompt>" --inline --prompt "$prompt"
expect_trace grok 'grok <--permission-mode> <bypassPermissions>' --inline
expect_trace grok "grok <--permission-mode> <bypassPermissions> <--> <$prompt>" --inline --prompt "$prompt"
expect_trace codex 'codex <--approve-for-me>' --inline
expect_trace codex "codex <--approve-for-me> <--> <$prompt>" --inline --prompt "$prompt"
expect_trace omp 'omp <--auto-approve>' --inline
expect_trace omp "omp <--auto-approve> <--> <$prompt>" --inline --prompt "$prompt"
expect_trace pi 'pi' --inline
expect_trace pi "pi <$prompt>" --inline --prompt "$prompt"

if [[ "$flavor" == "stable" ]]; then
  expect_trace gemini 'gemini <--yolo>' --inline
  expect_trace gemini "gemini <--yolo> <--prompt-interactive> <$prompt>" --inline --prompt "$prompt"
  selectors=9
else
  expect_trace agy 'agy <--dangerously-skip-permissions>' --inline
  expect_trace agy "agy <--dangerously-skip-permissions> <--prompt-interactive> <$prompt>" --inline --prompt "$prompt"
  expect_trace ori 'ori <code>' --inline
  expect_trace ori "ori <code> <--interactive> <--prompt> <$prompt>" --inline --prompt "$prompt"
  selectors=10
fi

expect_trace codex \
  'omarchy-launch-tui <--app-id=org.omarchy.agent> <codex> <--approve-for-me>'

expect_failure \
  'Choose default agent with: omarchy default agent <name>' \
  env TOOLPORT_TEST_SELECTOR= "$launcher" --inline

expect_failure \
  'codex is not installed' \
  env TOOLPORT_TEST_SELECTOR=codex TOOLPORT_TEST_MISSING=1 "$launcher" --inline

expect_failure \
  'Unexpected argument: release-fixture' \
  env TOOLPORT_TEST_SELECTOR=codex "$launcher" release-fixture

if find "$test_home" -mindepth 1 -maxdepth 1 ! -name Work -print -quit | rg -q .; then
  echo "error: launcher wrote unexpected state into the isolated home" >&2
  find "$test_home" -mindepth 1 -maxdepth 3 -print >&2
  exit 1
fi

echo "Omarchy $flavor launcher contract passed for $selectors selectors"
