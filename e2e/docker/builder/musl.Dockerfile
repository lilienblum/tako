FROM rust:1.98-alpine@sha256:a10e64dd139b7387337c7fbe8aca31b959b57b2fd4c8ae20a02cf1d6ea424dce

RUN apk add --no-cache \
  build-base \
  cmake \
  perl \
  pkgconfig \
  vips-dev

WORKDIR /workspace
