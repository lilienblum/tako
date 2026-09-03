FROM rust:1.98-bullseye@sha256:4730e387a220a08a365c77da3096544dde214f9d796c16284d4be45438cad4a9

RUN apt-get update \
  && apt-get install -y --no-install-recommends \
    cmake \
    libssl-dev \
    libvips-dev \
    pkg-config \
  && rm -rf /var/lib/apt/lists/*

WORKDIR /workspace
