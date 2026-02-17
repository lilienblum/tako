# syntax=docker/dockerfile:1.6

# Build Linux tako-server artifacts for both musl and glibc.

FROM rust:1.89-alpine AS builder-musl

RUN apk add --no-cache \
    musl-dev \
    clang \
    lld \
    cmake \
    g++ \
    pkgconfig \
    perl \
    make \
    openssl-dev \
    sqlite-dev \
    sqlite-static

WORKDIR /work

# Copy only Rust workspace inputs needed to build `tako-server`.
COPY Cargo.toml Cargo.lock ./
COPY tako-core/Cargo.toml tako-core/Cargo.toml
COPY tako-socket/Cargo.toml tako-socket/Cargo.toml
COPY tako/Cargo.toml tako/Cargo.toml
COPY tako-server/Cargo.toml tako-server/Cargo.toml

COPY tako-core/src tako-core/src
COPY tako-socket/src tako-socket/src
COPY tako/src tako/src
COPY tako-server/src tako-server/src

RUN cargo build -p tako-server --release


FROM rust:1.89-bookworm AS builder-glibc

RUN apt-get update \
    && apt-get install -y --no-install-recommends \
        ca-certificates \
        clang \
        lld \
        cmake \
        g++ \
        pkg-config \
        perl \
        make \
        libssl-dev \
        libsqlite3-dev \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /work

# Copy only Rust workspace inputs needed to build `tako-server`.
COPY Cargo.toml Cargo.lock ./
COPY tako-core/Cargo.toml tako-core/Cargo.toml
COPY tako-socket/Cargo.toml tako-socket/Cargo.toml
COPY tako/Cargo.toml tako/Cargo.toml
COPY tako-server/Cargo.toml tako-server/Cargo.toml

COPY tako-core/src tako-core/src
COPY tako-socket/src tako-socket/src
COPY tako/src tako/src
COPY tako-server/src tako-server/src

RUN cargo build -p tako-server --release


FROM alpine:3.20 AS tako-server-artifact-musl

COPY --from=builder-musl /work/target/release/tako-server /tako-server
RUN sha256sum /tako-server > /tako-server.sha256


FROM debian:bookworm-slim AS tako-server-artifact-glibc

COPY --from=builder-glibc /work/target/release/tako-server /tako-server
RUN sha256sum /tako-server > /tako-server.sha256
