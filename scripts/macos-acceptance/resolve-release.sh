#!/usr/bin/env bash
# Resolve the release under test and the published release before it.
# usage: resolve-release.sh [tag]   (empty means the latest published release)
# Prints tag=..., previous=... and latest=... lines for $GITHUB_OUTPUT/$GITHUB_ENV.
# latest is "true" when the tag is the current latest release (what the
# Homebrew tap and the in-app updater offer).
set -euo pipefail
REPO="${GITHUB_REPOSITORY:-btsouth/toolport}"
TAG="${1:-}"
# Every published, non-draft, non-prerelease release, newest first. --paginate
# walks all pages; each page prints "<published_at> <tag>" lines, sorted here.
releases="$(gh api --paginate "repos/$REPO/releases?per_page=100" \
  --jq '.[] | select(.draft == false and .prerelease == false) | "\(.published_at) \(.tag_name)"' |
  sort -r | awk '{print $2}')"
[[ -n "$releases" ]] || { echo "::error::no published releases found for $REPO" >&2; exit 1; }
latest="$(gh api "repos/$REPO/releases/latest" --jq .tag_name)"
[[ -n "$TAG" ]] || TAG="$latest"
if ! grep -qx -- "$TAG" <<<"$releases"; then
  echo "::error::$TAG is not a published release of $REPO" >&2
  exit 1
fi
previous="$(awk -v t="$TAG" 'found { print; exit } $0 == t { found = 1 }' <<<"$releases")"
echo "tag=$TAG"
echo "previous=$previous"
echo "latest=$([[ "$TAG" == "$latest" ]] && echo true || echo false)"
