# syntax=docker/dockerfile:1.7

ARG RUST_VERSION=1.98.0

FROM rust:${RUST_VERSION}-bookworm AS builder

WORKDIR /build

COPY Cargo.toml Cargo.lock rust-toolchain.toml ./
COPY migrations ./migrations
COPY src ./src

RUN --mount=type=cache,target=/usr/local/cargo/registry,sharing=locked \
    --mount=type=cache,target=/usr/local/cargo/git,sharing=locked \
    --mount=type=cache,target=/build/target,sharing=locked \
    cargo build --locked --release --bins \
    && install -D -m 0755 target/release/dm-api /out/dm-api \
    && install -D -m 0755 target/release/dm-worker /out/dm-worker \
    && install -D -m 0755 target/release/dm-migrate /out/dm-migrate

FROM debian:bookworm-slim AS runtime

RUN apt-get update \
    && apt-get install --yes --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/* \
    && groupadd --gid 10001 silicon \
    && useradd --uid 10001 --gid silicon --no-create-home \
        --home-dir /nonexistent --shell /usr/sbin/nologin silicon

COPY --from=builder --chown=10001:10001 /out/ /usr/local/bin/

WORKDIR /app
USER 10001:10001

ENV DM_BIND_ADDR=0.0.0.0:8080

EXPOSE 8080
STOPSIGNAL SIGTERM

CMD ["dm-api"]
