set working-directory := ".."

ARTIFACTS_DIR := "dist"

tako:
    cargo build -p tako --bin tako --release

tako-linux:
    mkdir -p {{ ARTIFACTS_DIR }}
    for arch in amd64 arm64; do \
        docker buildx build -f docker/build.Dockerfile \
            --platform linux/$arch \
            --target tako-artifact-glibc \
            --output type=local,dest={{ ARTIFACTS_DIR }}/linux-$arch-glibc \
            . || exit 1; \
        if [ "$arch" = "amd64" ]; then outarch="x86_64"; else outarch="aarch64"; fi; \
        cp {{ ARTIFACTS_DIR }}/linux-$arch-glibc/tako {{ ARTIFACTS_DIR }}/tako-linux-$outarch || exit 1; \
        cp {{ ARTIFACTS_DIR }}/linux-$arch-glibc/tako.sha256 {{ ARTIFACTS_DIR }}/tako-linux-$outarch.sha256 || exit 1; \
        rm -rf {{ ARTIFACTS_DIR }}/linux-$arch-glibc; \
    done
    echo "Built binaries:"
    ls -la {{ ARTIFACTS_DIR }}/tako-linux-*

# Build a single tako-server artifact.
# arch: amd64 | arm64
# libc: musl | glibc
# For the default `testbed.Dockerfile` container from `just testbed::create`
# (`DOCKER_PLATFORM=linux/arm64` on Alpine), use: `just build::tako-server arm64 musl`.
tako-server arch libc:
    mkdir -p {{ ARTIFACTS_DIR }}
    case "{{ arch }}" in amd64|arm64) ;; *) echo "Invalid arch '{{ arch }}' (expected: amd64 or arm64)" >&2; exit 1 ;; esac
    case "{{ libc }}" in musl|glibc) ;; *) echo "Invalid libc '{{ libc }}' (expected: musl or glibc)" >&2; exit 1 ;; esac
    docker buildx build -f docker/build.Dockerfile \
        --platform linux/{{ arch }} \
        --target tako-server-artifact-{{ libc }} \
        --output type=local,dest={{ ARTIFACTS_DIR }}/linux-{{ arch }}-{{ libc }} \
        .
    if [ "{{ arch }}" = "amd64" ]; then outarch="x86_64"; else outarch="aarch64"; fi; \
        cp {{ ARTIFACTS_DIR }}/linux-{{ arch }}-{{ libc }}/tako-server {{ ARTIFACTS_DIR }}/tako-server-linux-$outarch-{{ libc }}; \
        cp {{ ARTIFACTS_DIR }}/linux-{{ arch }}-{{ libc }}/tako-server.sha256 {{ ARTIFACTS_DIR }}/tako-server-linux-$outarch-{{ libc }}.sha256; \
        rm -rf {{ ARTIFACTS_DIR }}/linux-{{ arch }}-{{ libc }}; \
        echo "Built binaries:"; \
        ls -la {{ ARTIFACTS_DIR }}/tako-server-linux-$outarch-{{ libc }} {{ ARTIFACTS_DIR }}/tako-server-linux-$outarch-{{ libc }}.sha256

tako-server-all:
    for libc in musl glibc; do \
        for arch in amd64 arm64; do \
            just build::tako-server "$arch" "$libc" || exit 1; \
        done; \
    done
