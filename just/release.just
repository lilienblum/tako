set working-directory := ".."

BUILDER_IMAGE_REGISTRY := "ghcr.io/lilienblum"

# Publish all Rust crates in dependency order.
cargo:
    cargo publish -p tako-core
    cargo publish -p tako-socket
    cargo publish -p tako-server
    cargo publish -p tako

cargo-one package:
    cargo publish -p {{package}}

npm:
    cd sdk && npm publish

builder-images version registry=BUILDER_IMAGE_REGISTRY:
    docker buildx build -f docker/tako-builder-musl.Dockerfile \
        --platform linux/amd64,linux/arm64 \
        -t {{ registry }}/tako-builder-musl:{{ version }} \
        -t {{ registry }}/tako-builder-musl:latest \
        --push .
    docker buildx build -f docker/tako-builder-glibc.Dockerfile \
        --platform linux/amd64,linux/arm64 \
        -t {{ registry }}/tako-builder-glibc:{{ version }} \
        -t {{ registry }}/tako-builder-glibc:latest \
        --push .

all: cargo npm
