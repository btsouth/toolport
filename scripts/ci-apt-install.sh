#!/usr/bin/env bash
# Install apt packages on a GitHub Ubuntu runner, tolerating a flaky mirror.
#
# `apt-get update` on the hosted runners intermittently stops responding rather
# than failing. It hung three separate jobs on one pull request (PR #812), each
# time burning the job's whole timeout without ever reaching a compiler. An
# unbounded hang is the worst shape available: it wastes the full budget, and a
# run stuck in progress also blocks `gh run rerun --failed` on the jobs that
# genuinely failed.
#
# So bound each attempt and retry. A bad mirror costs seconds instead of the job.
# `timeout` sends SIGTERM at the limit, which apt handles cleanly.
#
# The download side of `apt-get install` needs the same treatment, and only got
# it later: with `update` bounded, a throttled mirror moved the hang one step
# down. On PR #813 the mirror served ~12 kB/s, so the install crept through 130
# small packages and then died inside the 27 MB libwebkit2gtk-4.1-0, twice,
# each time burning the full 45-minute budget. Only the heavy WebKitGTK job hit
# it; the jobs asking for four packages finished in three minutes.
#
# The bounds stop a hang, but they cannot make a mirror that serves small debs
# at seconds each finish the WebKitGTK set inside three attempts. That failed
# "Rust Clippy (desktop)" on main and twice on PR #1419. So the downloads carry
# across runs: .github/actions/apt-install restores a directory of earlier debs
# and saves it again afterwards, failed runs included. Set
# TOOLPORT_APT_DEB_CACHE to that directory. Each run then only fetches what
# no earlier run got, and a slow-but-progressing mirror finishes eventually.
#
# Usage: scripts/ci-apt-install.sh <package>...
set -euo pipefail

if [ "$#" -eq 0 ]; then
  echo "usage: $0 <package>..." >&2
  exit 2
fi

attempts=3
per_attempt_secs=120
# Downloads are bulkier than an index refresh, so they get their own budget:
# a healthy WebKitGTK fetch is well under two minutes.
#
# Worst case, counted honestly: `timeout --kill-after` can spend budget+30s per
# call, the download loop refreshes the index before each retry, and only the
# gaps between attempts sleep. That is 3x150s + 2x5s = 460s of update, then
# 3x330s + 2x150s + 2x5s = 1300s of download: about 29 minutes.
#
# That is a backstop, not a plan - it needs every attempt to burn its full bound
# and still come good. But it is not comfortably inside every caller either: the
# WebKitGTK job allows 45 minutes, while the four-package jobs in ci.yml and
# docker-publish.yml allow 30. Those jobs pull a handful of small debs, so they
# reach the bounds only if the mirror is dead, in which case they were losing the
# job anyway. Re-check this arithmetic before raising either budget.
#
# The deb cache does not change these bounds. Seeding it costs one hash and one
# index lookup per cached deb after the update, a few seconds for the WebKitGTK
# set, and copying it back on exit is a local copy of at most a few hundred MB.
download_attempt_secs=300
# SIGTERM is the polite ask. An apt wedged on a dead socket can sit through it,
# which would put the unbounded wait straight back; follow up with SIGKILL.
kill_after_secs=30

# Let apt do its own failover before we cut it off.
#
# The runner image already points apt at a three-mirror list through
# `mirror+file:/etc/apt/apt-mirrors.txt` - azure.archive.ubuntu.com first, then
# archive.ubuntu.com, then security.ubuntu.com - so a dead primary is something
# apt can recover from unaided. It never got the chance: the image sets no
# acquire timeout, apt's default idle timeout is at least as long as the bound
# below, and so the SIGTERM always landed first. Three attempts against a silent
# azure mirror produced no error, no failover, and six minutes gone, on three
# pull requests in one evening (#813, #815, #816).
#
# So give apt a much shorter fuse than our own and the fallback engages inside a
# single attempt, turning the outage into a few wasted seconds.
#
# This is an INACTIVITY timeout, not a transfer budget: it measures the gap
# between bytes, so it does not punish a slow-but-progressing mirror. The
# throttled ~12 kB/s mirror that broke PR #817 keeps resetting it and is left to
# the outer bound, which is the right tool for that failure.
apt_conf=/etc/apt/apt.conf.d/99toolport-ci-acquire
if ! printf '%s\n' \
  'Acquire::http::Timeout "20";' \
  'Acquire::https::Timeout "20";' \
  'Acquire::Retries "3";' | sudo tee "$apt_conf" >/dev/null; then
  # Not fatal: without it apt keeps its own defaults and the loops below still
  # bound the damage. Say so rather than failing a build over a tuning file.
  echo "could not write ${apt_conf}; apt keeps its default timeouts" >&2
fi

deb_cache="${TOOLPORT_APT_DEB_CACHE:-}"
archives=/var/cache/apt/archives

# Hand apt the cached debs it can trust. A cache is written by other runs, so a
# deb goes in only when its SHA256 matches the record the just-updated index has
# for the package, version and architecture its filename names. apt looks debs
# up by filename and checks only their size, so the filename is what has to
# match. Anything that fails that check (an old version, a corrupt file) is left
# out, which also prunes the cache when it is saved again. A valid deb from
# another package set passes it and stays, so caches restored across sets carry
# their union; seeding never limits itself to the requested packages.
#
# Partial downloads are never carried across runs. A copied partial gets a new
# mtime, apt sends that as If-Range, and the mirror answers with the whole file
# from byte zero, so it bought nothing and left junk in the cache. Inside one
# run apt still resumes its partials across the bounded attempts below, because
# /var/cache/apt/archives/partial stays put.
seed_deb_cache() {
  local deb name pkg ver arch sum records seeded=0 skipped=0
  for deb in "$deb_cache"/*.deb; do
    [ -f "$deb" ] || continue
    name=$(basename "$deb" .deb)
    IFS=_ read -r pkg ver arch <<<"$name"
    ver=${ver//%3a/:}
    sum=$(sha256sum "$deb" | cut -d' ' -f1)
    # Captured rather than piped into grep -q, which can exit early and fail the
    # pipeline on SIGPIPE under pipefail.
    records=$(apt-cache show "${pkg}:${arch}=${ver}" 2>/dev/null || true)
    if [ -n "$arch" ] && grep -qx "SHA256: ${sum}" <<<"$records"; then
      sudo cp "$deb" "$archives/"
      seeded=$((seeded + 1))
    else
      skipped=$((skipped + 1))
    fi
  done
  echo "deb cache: ${seeded} debs reused, ${skipped} not in the current index"
}

# Copy the complete debs apt holds back into the cache directory, on every exit,
# so a run the mirror defeated still leaves its progress for the next one. Every
# deb there passed a SHA256 check against the index: the seed step's, or apt's
# own before it moves a download out of partial/. Rebuilt rather than merged, so
# stale debs dropped by the seed step do not come back.
save_deb_cache() {
  [ -n "$deb_cache" ] || return 0
  rm -rf "$deb_cache"
  mkdir -p "$deb_cache"
  sudo find "$archives" -maxdepth 1 -name '*.deb' -exec cp -t "$deb_cache/" {} + || true
  sudo chown -R "$(id -u):$(id -g)" "$deb_cache" || true
  echo "deb cache: kept $(find "$deb_cache" -maxdepth 1 -name '*.deb' | wc -l) debs"
}

# Saving is best effort and must not change the result: keep the status the
# script was exiting with, success or failure.
save_deb_cache_on_exit() {
  local status=$?
  save_deb_cache || echo "deb cache: could not save" >&2
  exit "$status"
}

if [ -n "$deb_cache" ]; then
  mkdir -p "$deb_cache"
fi

run_apt() {
  local budget="$1"
  shift
  sudo timeout --kill-after="$kill_after_secs" "$budget" apt-get "$@"
}

updated=
for attempt in $(seq 1 "$attempts"); do
  if run_apt "$per_attempt_secs" update; then
    updated=1
    break
  fi
  # Only sleep between attempts. After the last one the caller is about to see
  # the failure, and five more seconds of dead air buys nothing.
  if [ "$attempt" -lt "$attempts" ]; then
    echo "apt-get update attempt ${attempt}/${attempts} failed or hung; retrying" >&2
    sleep 5
  fi
done

if [ -z "$updated" ]; then
  echo "apt-get update failed ${attempts} times; see the apt error above" >&2
  exit 1
fi

# The cache is rebuilt from what apt holds only once seeding has run. Before
# that, apt's archive holds none of the restored debs, so a run whose index
# refresh failed would replace a warm cache with an empty one.
if [ -n "$deb_cache" ]; then
  seed_deb_cache
  trap save_deb_cache_on_exit EXIT
fi

# Fetch first, on a bound, so a stalled mirror is interrupted and retried. This
# is safe to cut off mid-flight: apt keeps each completed .deb in
# /var/cache/apt/archives, so a retry resumes with only what is still missing
# instead of starting the whole set again.
downloaded=
for attempt in $(seq 1 "$attempts"); do
  # Refresh the index before each retry, never on the first pass. A mirror that
  # publishes a new version between our update and our download 404s every
  # attempt on the version we were told to ask for, and retrying the same stale
  # plan just burns the budget three times over.
  if [ "$attempt" -gt 1 ] && ! run_apt "$per_attempt_secs" update; then
    echo "index refresh before download attempt ${attempt} failed; trying the download anyway" >&2
  fi
  if run_apt "$download_attempt_secs" install -y --download-only "$@"; then
    downloaded=1
    break
  fi
  if [ "$attempt" -lt "$attempts" ]; then
    echo "apt-get download attempt ${attempt}/${attempts} failed or hung; retrying" >&2
    sleep 5
  fi
done

if [ -z "$downloaded" ]; then
  echo "apt-get could not download the packages after ${attempts} attempts; see the apt error above" >&2
  exit 1
fi

# Install straight from the cache the loop above filled. `--no-download` is what
# makes "offline" true rather than merely likely: without it this is a fresh
# acquire session, so one truncated or superseded .deb sends it back to the
# mirror unbounded, which is the exact hang this script exists to stop. A
# missing file now fails fast and loudly instead. Deliberately no `-m`: that
# would drop the missing package and install a quiet subset.
#
# No timeout here on purpose. There is no network left to stall on, and a
# SIGTERM landing inside dpkg is the one interruption that leaves a
# half-configured system needing `dpkg --configure -a`.
sudo apt-get install -y --no-download "$@"
