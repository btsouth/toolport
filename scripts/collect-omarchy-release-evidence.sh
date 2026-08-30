#!/usr/bin/env bash
set -euo pipefail

for command in jq pacman timeout; do
  command -v "$command" >/dev/null || {
    echo "error: $command is required" >&2
    exit 1
  }
done

package=$(pacman -Q omarchy 2>/dev/null || pacman -Q omarchy-dev 2>/dev/null || true)
selected=$(omarchy-default-agent 2>/dev/null || true)
selector_command=$(command -v omarchy-default-agent 2>/dev/null || true)
selectors=""
if [[ -n "$selector_command" && -r "$selector_command" ]]; then
  selectors=$(sed -n 's/^# omarchy:args=\[\(.*\)\]$/\1/p' "$selector_command" | head -n 1)
fi

channel_from_file() {
  local file="$1"
  [[ -r "$file" ]] || return 0
  if grep -q 'stable-mirror\.omarchy\.org\|pkgs\.omarchy\.org/stable/' "$file"; then
    printf stable
  elif grep -q 'rc-mirror\.omarchy\.org\|pkgs\.omarchy\.org/rc/' "$file"; then
    printf rc
  elif grep -q 'mirror\.omarchy\.org\|pkgs\.omarchy\.org/edge/' "$file"; then
    printf edge
  fi
}

mirror_channel=$(channel_from_file /etc/pacman.d/mirrorlist)
package_channel=$(channel_from_file /etc/pacman.conf)

version_for() {
  local selector="$1"
  case "$selector" in
    omp | crush) timeout 8 "$selector" -v 2>&1 ;;
    ori) timeout 8 ori --version 2>/dev/null | jq -r '.data.version // empty' ;;
    *) timeout 8 "$selector" --version 2>&1 ;;
  esac
}

agents='[]'
while IFS='|' read -r selector client_id; do
  path=$(command -v "$selector" 2>/dev/null || true)
  display_path=${path/#${HOME:-__no_home__}/\$HOME}
  if [[ -n "$path" ]]; then
    version=$(version_for "$selector" | tr '\n' ' ' | sed 's/[[:space:]]\+/ /g; s/^ //; s/ $//' || true)
  else
    version=""
  fi
  supported=true
  if [[ "$selector" == "ori" ]]; then
    supported=false
  fi
  agents=$(jq \
    --arg selector "$selector" \
    --arg clientId "$client_id" \
    --arg path "$display_path" \
    --arg version "$version" \
    --argjson installed "$([[ -n "$path" ]] && printf true || printf false)" \
    --argjson supported "$supported" \
    '. + [{selector: $selector, clientId: ($clientId | if length > 0 then . else null end), installed: $installed, path: ($path | if length > 0 then . else null end), version: ($version | if length > 0 then . else null end), supported: $supported}]' \
    <<<"$agents")
done <<'AGENTS'
pi|pi
omp|omp
opencode|opencode
ori|
claude|claude-code
codex|codex
grok|grok
agy|antigravity
gemini|gemini-cli
copilot|github-copilot-cli
crush|crush
AGENTS

jq -n \
  --arg collectedAt "$(date --utc +%Y-%m-%dT%H:%M:%SZ)" \
  --arg package "$package" \
  --arg selectedAgent "$selected" \
  --arg selectorCapabilities "$selectors" \
  --arg mirrorChannel "$mirror_channel" \
  --arg packageChannel "$package_channel" \
  --arg desktop "${XDG_CURRENT_DESKTOP:-}" \
  --arg sessionType "${XDG_SESSION_TYPE:-}" \
  --arg hyprland "${HYPRLAND_INSTANCE_SIGNATURE:+present}" \
  --argjson agents "$agents" \
  '{
    collectedAt: $collectedAt,
    omarchy: {
      package: ($package | select(length > 0)),
      selectedAgent: ($selectedAgent | select(length > 0)),
      selectorCapabilities: ($selectorCapabilities | split("|") | map(select(length > 0))),
      mirrorChannel: ($mirrorChannel | select(length > 0)),
      packageChannel: ($packageChannel | select(length > 0))
    },
    session: {
      desktop: ($desktop | select(length > 0)),
      type: ($sessionType | select(length > 0)),
      hyprland: ($hyprland == "present")
    },
    agents: $agents
  }'
