#!/usr/bin/env bash
# Resolve the release under test and the published release before it.
# usage: resolve-release.sh [tag]   (empty means the latest published release)
# Prints tag=... and previous=... lines for $GITHUB_OUTPUT.
set -euo pipefail
REPO="${GITHUB_REPOSITORY:-btsouth/toolport}"
TAG="${1:-}"
# Published, non-draft, non-prerelease releases, newest first.
releases="$(gh api "repos/$REPO/releases?per_page=50" \
  --jq '[.[] | select(.draft == false and .prerelease == false)] | sort_by(.published_at) | reverse | .[].tag_name')"
if [[ -z "$TAG" ]]; then
  TAG="$(gh api "repos/$REPO/releases/latest" --jq .tag_name)"
fi
if ! grep -qx "$TAG" <<<"$releases"; then
  echo "::error::$TAG is not a published release of $REPO" >&2
  exit 1
fi
previous="$(awk -v t="$TAG" 'found { print; exit } $0 == t { found = 1 }' <<<"$releases")"
echo "tag=$TAG"
echo "previous=$previous"
