# syntax=docker/dockerfile:1.6

# Build Linux tako and tako-server artifacts for both musl and glibc.

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
ARG TAKO_CANARY_SHA
ENV TAKO_CANARY_SHA=$TAKO_CANARY_SHA
ARG BUILD_TAKO=1
ARG BUILD_TAKO_SERVER=1

# Copy only Rust workspace inputs needed to build release binaries.
COPY Cargo.toml Cargo.lock ./
COPY tako-core/Cargo.toml tako-core/Cargo.toml
COPY tako-socket/Cargo.toml tako-socket/Cargo.toml
COPY tako/Cargo.toml tako/Cargo.toml
COPY tako-server/Cargo.toml tako-server/Cargo.toml

COPY tako-core/src tako-core/src
COPY tako-socket/src tako-socket/src
COPY tako/src tako/src
COPY tako-server/src tako-server/src

RUN if [ "$BUILD_TAKO" = "1" ]; then cargo build -p tako --bin tako --release; fi \
    && if [ "$BUILD_TAKO_SERVER" = "1" ]; then cargo build -p tako-server --release; fi


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
ARG TAKO_CANARY_SHA
ENV TAKO_CANARY_SHA=$TAKO_CANARY_SHA
ARG BUILD_TAKO=1
ARG BUILD_TAKO_SERVER=1

# Copy only Rust workspace inputs needed to build release binaries.
COPY Cargo.toml Cargo.lock ./
COPY tako-core/Cargo.toml tako-core/Cargo.toml
COPY tako-socket/Cargo.toml tako-socket/Cargo.toml
COPY tako/Cargo.toml tako/Cargo.toml
COPY tako-server/Cargo.toml tako-server/Cargo.toml

COPY tako-core/src tako-core/src
COPY tako-socket/src tako-socket/src
COPY tako/src tako/src
COPY tako-server/src tako-server/src

RUN if [ "$BUILD_TAKO" = "1" ]; then cargo build -p tako --bin tako --release; fi \
    && if [ "$BUILD_TAKO_SERVER" = "1" ]; then cargo build -p tako-server --release; fi


FROM alpine:3.20 AS tako-artifact-musl

COPY --from=builder-musl /work/target/release/tako /tako
RUN sha256sum /tako > /tako.sha256


FROM debian:bookworm-slim AS tako-artifact-glibc

COPY --from=builder-glibc /work/target/release/tako /tako
RUN sha256sum /tako > /tako.sha256


FROM debian:bookworm-slim AS tako-and-server-artifact-glibc

COPY --from=builder-glibc /work/target/release/tako /tako
COPY --from=builder-glibc /work/target/release/tako-server /tako-server
RUN sha256sum /tako > /tako.sha256 \
    && sha256sum /tako-server > /tako-server.sha256


FROM alpine:3.20 AS tako-server-artifact-musl

COPY --from=builder-musl /work/target/release/tako-server /tako-server
RUN sha256sum /tako-server > /tako-server.sha256


FROM debian:bookworm-slim AS tako-server-artifact-glibc

COPY --from=builder-glibc /work/target/release/tako-server /tako-server
RUN sha256sum /tako-server > /tako-server.sha256
