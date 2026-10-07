# Runtime-only headless Toolport gateway image.
# CI builds `toolport-gateway` with cached Rust (see docker-publish.yml) and
# copies the binary in as `toolport-gateway-bin` before `docker build`.
# For a from-source local build, use: docker build -f Dockerfile.source .

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends \
        ca-certificates libssl3 libdbus-1-3 curl \
    && rm -rf /var/lib/apt/lists/*
WORKDIR /data
COPY toolport-gateway-bin /usr/local/bin/toolport-gateway
COPY docker-entrypoint.sh /usr/local/bin/docker-entrypoint.sh
RUN chmod 755 /usr/local/bin/toolport-gateway /usr/local/bin/docker-entrypoint.sh \
    && useradd --system --uid 10001 --home-dir /data toolport \
    && chown toolport:toolport /data

# The runtime user is fixed at uid 10001. A host-owned bind mount must be owned
# by that uid (see the entrypoint warning) or the gateway cannot load the
# registry; the named volume in docker-compose.example.yml is initialized with
# the image's ownership and just works.
USER toolport
ENV CONDUIT_HTTP=8765
ENV CONDUIT_HTTP_HOST=0.0.0.0
ENV CONDUIT_REGISTRY=/data/registry.json
EXPOSE 8765
VOLUME ["/data"]

# Readiness, not liveness: /healthz answers 200 only once the registry loaded,
# and 503 when the gateway is serving the cached catalog only (INST-02). It is
# unauthenticated and data-free so the healthcheck needs no token.
HEALTHCHECK --interval=30s --timeout=5s --start-period=10s --retries=3 \
    CMD curl -fsS -o /dev/null http://127.0.0.1:8765/healthz || exit 1

ENTRYPOINT ["docker-entrypoint.sh", "toolport-gateway", "--http", "8765"]
