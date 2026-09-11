# syntax=docker/dockerfile:1.7

ARG RUST_VERSION=1.96.1
FROM rust:${RUST_VERSION}-bookworm AS builder
ARG CARGO_BUILD_JOBS=2

WORKDIR /workspace
COPY . .

RUN --mount=type=cache,target=/usr/local/cargo/registry,sharing=locked \
    --mount=type=cache,target=/usr/local/cargo/git,sharing=locked \
    --mount=type=cache,target=/workspace/target,sharing=locked \
    cargo build --locked --release --jobs "${CARGO_BUILD_JOBS}" --package rill3 --bin rill3 \
    && cp /workspace/target/release/rill3 /workspace/rill3-release

FROM debian:bookworm-slim AS runtime

RUN apt-get update \
    && apt-get install --no-install-recommends -y ca-certificates curl \
    && rm -rf /var/lib/apt/lists/* \
    && groupadd --gid 10001 rill3 \
    && useradd --uid 10001 --gid rill3 --no-create-home --home-dir /nonexistent --shell /usr/sbin/nologin rill3

COPY --from=builder --chown=10001:10001 /workspace/rill3-release /usr/local/bin/rill3
COPY --from=builder --chown=10001:10001 /workspace/static /app/static

USER 10001:10001
WORKDIR /app

ENV RILL3_BIND_ADDR=0.0.0.0:3000 \
    RILL3_ENVIRONMENT=production \
    RILL3_DB_MAX_CONNECTIONS=20

EXPOSE 3000
STOPSIGNAL SIGTERM

HEALTHCHECK --interval=30s --timeout=3s --start-period=10s --retries=3 \
    CMD ["curl", "--fail", "--silent", "--show-error", "http://127.0.0.1:3000/health/live"]

ENTRYPOINT ["/usr/local/bin/rill3"]
CMD ["server"]
