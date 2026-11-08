# syntax=docker/dockerfile:1

FROM rust:1-slim-trixie AS builder

WORKDIR /build
COPY . .

RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/build/target \
    cargo build --locked --release --package backend --bin backend \
    && cp /build/target/release/backend /build/backend

FROM debian:trixie-slim AS runtime

ARG APP_UID=65532
ARG APP_GID=65532

WORKDIR /app
COPY --from=builder /build/backend /usr/local/bin/backend

USER ${APP_UID}:${APP_GID}

ENTRYPOINT ["/usr/local/bin/backend"]
STOPSIGNAL SIGTERM
