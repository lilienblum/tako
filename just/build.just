set working-directory := ".."

ARTIFACTS_DIR := "dist"

tako:
    cargo build -p tako --bin tako --release

_require-zigbuild:
    @if ! command -v cargo-zigbuild >/dev/null 2>&1; then \
        echo "error: cargo-zigbuild is required (install: cargo install cargo-zigbuild)" >&2; \
        echo "       also install zig: brew install zig (macOS) or pip3 install ziglang" >&2; \
        exit 1; \
    fi

tako-linux: _require-zigbuild
    mkdir -p {{ ARTIFACTS_DIR }}
    rustup target add x86_64-unknown-linux-gnu aarch64-unknown-linux-gnu 2>/dev/null || true
    cargo zigbuild -p tako --bin tako --bin tako-dev-server --release --target x86_64-unknown-linux-gnu
    cargo zigbuild -p tako --bin tako --bin tako-dev-server --release --target aarch64-unknown-linux-gnu
    cp target/x86_64-unknown-linux-gnu/release/tako {{ ARTIFACTS_DIR }}/tako-linux-x86_64
    cp target/x86_64-unknown-linux-gnu/release/tako-dev-server {{ ARTIFACTS_DIR }}/tako-dev-server-linux-x86_64
    cp target/aarch64-unknown-linux-gnu/release/tako {{ ARTIFACTS_DIR }}/tako-linux-aarch64
    cp target/aarch64-unknown-linux-gnu/release/tako-dev-server {{ ARTIFACTS_DIR }}/tako-dev-server-linux-aarch64
    @echo "Built binaries:"
    ls -la {{ ARTIFACTS_DIR }}/tako-linux-* {{ ARTIFACTS_DIR }}/tako-dev-server-linux-*

# Build a single tako-server artifact.
# arch: x86_64 | aarch64
# libc: musl | glibc
tako-server arch libc: _require-zigbuild
    mkdir -p {{ ARTIFACTS_DIR }}
    #!/usr/bin/env sh
    set -eu
    case "{{ arch }}" in x86_64|aarch64) ;; *) echo "Invalid arch '{{ arch }}' (expected: x86_64 or aarch64)" >&2; exit 1 ;; esac
    case "{{ libc }}" in musl|glibc) ;; *) echo "Invalid libc '{{ libc }}' (expected: musl or glibc)" >&2; exit 1 ;; esac
    case "{{ libc }}" in
        musl)  target="{{ arch }}-unknown-linux-musl" ;;
        glibc) target="{{ arch }}-unknown-linux-gnu" ;;
    esac
    rustup target add "$target" 2>/dev/null || true
    cargo zigbuild -p tako-server --release --target "$target"
    cp "target/$target/release/tako-server" "{{ ARTIFACTS_DIR }}/tako-server-linux-{{ arch }}-{{ libc }}"
    echo "Built binaries:"
    ls -la "{{ ARTIFACTS_DIR }}/tako-server-linux-{{ arch }}-{{ libc }}"

tako-server-all: _require-zigbuild
    for libc in musl glibc; do \
        for arch in x86_64 aarch64; do \
            just build::tako-server "$arch" "$libc" || exit 1; \
        done; \
    done
