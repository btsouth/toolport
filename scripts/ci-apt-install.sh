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
# Usage: scripts/ci-apt-install.sh <package>...
set -euo pipefail

if [ "$#" -eq 0 ]; then
  echo "usage: $0 <package>..." >&2
  exit 2
fi

attempts=3
per_attempt_secs=120

updated=
for attempt in $(seq 1 "$attempts"); do
  if sudo timeout "$per_attempt_secs" apt-get update; then
    updated=1
    break
  fi
  echo "apt-get update attempt ${attempt}/${attempts} failed or hung; retrying" >&2
  sleep 5
done

if [ -z "$updated" ]; then
  echo "apt-get update failed ${attempts} times; the mirror is unreachable" >&2
  exit 1
fi

sudo apt-get install -y "$@"
