# syntax=docker/dockerfile:1.6

# Build artifacts for Linux (musl) inside a container.

FROM rust:1.88-alpine AS builder

RUN apk add --no-cache \
    musl-dev \
    clang \
    lld \
    cmake \
    g++ \
    pkgconfig \
    perl \
    make \
    openssl-dev

WORKDIR /work

# Copy only Rust workspace inputs needed to build `tako-server`.
COPY Cargo.toml Cargo.lock ./
COPY tako-core/Cargo.toml tako-core/Cargo.toml
COPY tako-socket/Cargo.toml tako-socket/Cargo.toml
COPY tako/Cargo.toml tako/Cargo.toml
COPY tako-server/Cargo.toml tako-server/Cargo.toml
COPY tako-dev-server/Cargo.toml tako-dev-server/Cargo.toml

COPY tako-core/src tako-core/src
COPY tako-socket/src tako-socket/src
COPY tako/src tako/src
COPY tako-server/src tako-server/src
COPY tako-dev-server/src tako-dev-server/src

RUN cargo build -p tako-server --release


FROM alpine:3.20 AS tako-server-artifact

COPY --from=builder /work/target/release/tako-server /tako-server
RUN sha256sum /tako-server > /tako-server.sha256
