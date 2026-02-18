set working-directory := ".."

ARTIFACTS_DIR := "dist"

tako:
    cargo build -p tako --bin tako --release

tako-server:
    mkdir -p {{ ARTIFACTS_DIR }}
    for libc in musl glibc; do \
        for arch in amd64 arm64; do \
            docker buildx build -f docker/build.Dockerfile \
                --platform linux/$arch \
                --target tako-server-artifact-$libc \
                --output type=local,dest={{ ARTIFACTS_DIR }}/linux-$arch-$libc \
                . || exit 1; \
            if [ "$arch" = "amd64" ]; then outarch="x86_64"; else outarch="aarch64"; fi; \
            cp {{ ARTIFACTS_DIR }}/linux-$arch-$libc/tako-server {{ ARTIFACTS_DIR }}/tako-server-linux-$outarch-$libc || exit 1; \
            cp {{ ARTIFACTS_DIR }}/linux-$arch-$libc/tako-server.sha256 {{ ARTIFACTS_DIR }}/tako-server-linux-$outarch-$libc.sha256 || exit 1; \
            rm -rf {{ ARTIFACTS_DIR }}/linux-$arch-$libc; \
        done; \
    done
    echo "Built binaries:"
    ls -la {{ ARTIFACTS_DIR }}/tako-server-linux-*
