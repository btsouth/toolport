#!/bin/sh
# Entrypoint for the headless gateway image.
#
# The image runs as toolport (uid 10001), so it can only write a mounted /data
# that the runtime user owns. A host-owned bind mount (the common "mkdir data"
# quick start) is owned by the host user, and uid 10001 then cannot create the
# registry lock: the gateway starts, logs that it is serving cached tools only,
# and stays up. That is a silent degrade, so name it here with the exact uid and
# the one-line fix. The container keeps running so `docker inspect` can still
# report the unhealthy healthcheck (GET /healthz) rather than a bare exit.
set -eu

registry="${TOOLPORT_REGISTRY:-${CONDUIT_REGISTRY:-/data/registry.json}}"
registry_dir="$(dirname "$registry")"
mkdir -p "$registry_dir" 2>/dev/null || true

if [ ! -w "$registry_dir" ]; then
  echo "toolport-gateway: $registry_dir is not writable by the runtime user (uid 10001)." >&2
  echo "toolport-gateway: the gateway cannot create the registry lock and will serve cached tools only." >&2
  echo "toolport-gateway: fix the mount with 'sudo chown -R 10001:10001 <host dir>', or use the named volume in docker-compose.example.yml." >&2
fi

exec "$@"
