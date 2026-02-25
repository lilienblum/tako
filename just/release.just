set working-directory := ".."

BUILDER_IMAGE_REGISTRY := "ghcr.io/lilienblum"
TAKO_TAG_PREFIX := "tako-v"
TAKO_SERVER_TAG_PREFIX := "tako-server-v"
TAKO_CORE_TAG_PREFIX := "tako-core-v"
TAKO_SOCKET_TAG_PREFIX := "tako-socket-v"
SDK_TAG_PREFIX := "sdk-npm-v"
RELEASE_NOTES_DIR := "dist/release-notes"

# Release flow (local):
# - CLI:          just release tako [series]
# - Server:       just release tako-server [series]
# - SDK:          just release sdk [series]
# - Shared crates: just release tako-core / just release tako-socket
#
# If shared crates changed, release them first:
#   just release tako-core
#   just release tako-socket

_guard:
    @branch="$(git rev-parse --abbrev-ref HEAD)"; \
    if [ "$branch" != "master" ]; then \
        echo "error: releases must run from master (current branch: $branch)" >&2; \
        exit 1; \
    fi; \
    if [ -n "$(git status --porcelain)" ]; then \
        echo "error: working tree is not clean; commit or stash changes before releasing" >&2; \
        exit 1; \
    fi; \
    if ! git fetch --quiet --tags origin master; then \
        echo "error: failed to fetch origin/master; cannot verify branch freshness" >&2; \
        exit 1; \
    fi; \
    local_head="$(git rev-parse HEAD)"; \
    remote_head="$(git rev-parse origin/master)"; \
    if [ "$local_head" != "$remote_head" ]; then \
        echo "error: local master is not in sync with origin/master" >&2; \
        exit 1; \
    fi

_require-cargo-set-version:
    @if ! cargo set-version --help >/dev/null 2>&1; then \
        echo "error: cargo set-version is required (install cargo-edit)" >&2; \
        exit 1; \
    fi

_require-gh:
    @if ! gh --version >/dev/null 2>&1; then \
        echo "error: gh CLI is required for publishing GitHub releases" >&2; \
        exit 1; \
    fi; \
    if ! gh auth status >/dev/null 2>&1; then \
        echo "error: gh is not authenticated; run 'gh auth login'" >&2; \
        exit 1; \
    fi

_require-git-cliff:
    @if ! git cliff --version >/dev/null 2>&1; then \
        echo "error: git-cliff is required for release notes generation" >&2; \
        exit 1; \
    fi

_guard-shared-crates-released:
    @core_tag="$(git tag --list '{{ TAKO_CORE_TAG_PREFIX }}*' --sort=-v:refname | head -n 1 || true)"; \
    socket_tag="$(git tag --list '{{ TAKO_SOCKET_TAG_PREFIX }}*' --sort=-v:refname | head -n 1 || true)"; \
    core_changed=0; \
    socket_changed=0; \
    if [ -n "$core_tag" ]; then \
        if [ -n "$(git log --oneline "$core_tag..HEAD" -- tako-core/Cargo.toml tako-core/src | head -n 1)" ]; then core_changed=1; fi; \
    fi; \
    if [ -n "$socket_tag" ]; then \
        if [ -n "$(git log --oneline "$socket_tag..HEAD" -- tako-socket/Cargo.toml tako-socket/src | head -n 1)" ]; then socket_changed=1; fi; \
    fi; \
    if [ "$core_changed" -eq 1 ] || [ "$socket_changed" -eq 1 ]; then \
        echo "error: shared crate changes must be released before releasing tako/tako-server" >&2; \
        if [ "$core_changed" -eq 1 ]; then \
            if [ -n "$core_tag" ]; then \
                echo "• unreleased tako-core changes since $core_tag" >&2; \
            else \
                echo "• unreleased tako-core changes (no {{ TAKO_CORE_TAG_PREFIX }}* tag found)" >&2; \
            fi; \
        fi; \
        if [ "$socket_changed" -eq 1 ]; then \
            if [ -n "$socket_tag" ]; then \
                echo "• unreleased tako-socket changes since $socket_tag" >&2; \
            else \
                echo "• unreleased tako-socket changes (no {{ TAKO_SOCKET_TAG_PREFIX }}* tag found)" >&2; \
            fi; \
        fi; \
        echo "Release shared crates first:" >&2; \
        echo "  just release tako-core" >&2; \
        echo "  just release tako-socket" >&2; \
        exit 1; \
    fi

_prebuild-tako:
    @if [ "$(uname -s)" != "Darwin" ]; then \
        echo "error: release::tako must run on macOS to produce darwin installer artifacts" >&2; \
        exit 1; \
    fi
    @mkdir -p dist
    @echo "Prebuilding tako for macOS installer targets..."
    rustup target add aarch64-apple-darwin x86_64-apple-darwin
    cargo build -p tako --bin tako --release --target aarch64-apple-darwin
    cargo build -p tako --bin tako --release --target x86_64-apple-darwin
    @echo "Prebuilding tako for Linux targets..."
    just build::tako-linux
    @echo "Packaging tako release archives..."
    tmpdir="$(mktemp -d)"; \
    cp target/aarch64-apple-darwin/release/tako "$tmpdir/tako"; \
    tar -czf dist/tako-darwin-aarch64.tar.gz -C "$tmpdir" tako; \
    rm -rf "$tmpdir"; \
    tmpdir="$(mktemp -d)"; \
    cp target/x86_64-apple-darwin/release/tako "$tmpdir/tako"; \
    tar -czf dist/tako-darwin-x86_64.tar.gz -C "$tmpdir" tako; \
    rm -rf "$tmpdir"; \
    for arch in x86_64 aarch64; do \
        tmpdir="$(mktemp -d)"; \
        cp "dist/tako-linux-$arch" "$tmpdir/tako"; \
        tar -czf "dist/tako-linux-$arch.tar.gz" -C "$tmpdir" tako; \
        rm -rf "$tmpdir"; \
    done; \
    if command -v sha256sum >/dev/null 2>&1; then \
        sha256sum dist/tako-darwin-aarch64.tar.gz > dist/tako-darwin-aarch64.tar.gz.sha256; \
        sha256sum dist/tako-darwin-x86_64.tar.gz > dist/tako-darwin-x86_64.tar.gz.sha256; \
        sha256sum dist/tako-linux-x86_64.tar.gz > dist/tako-linux-x86_64.tar.gz.sha256; \
        sha256sum dist/tako-linux-aarch64.tar.gz > dist/tako-linux-aarch64.tar.gz.sha256; \
    else \
        shasum -a 256 dist/tako-darwin-aarch64.tar.gz > dist/tako-darwin-aarch64.tar.gz.sha256; \
        shasum -a 256 dist/tako-darwin-x86_64.tar.gz > dist/tako-darwin-x86_64.tar.gz.sha256; \
        shasum -a 256 dist/tako-linux-x86_64.tar.gz > dist/tako-linux-x86_64.tar.gz.sha256; \
        shasum -a 256 dist/tako-linux-aarch64.tar.gz > dist/tako-linux-aarch64.tar.gz.sha256; \
    fi

_prebuild-tako-server:
    @echo "Prebuilding tako-server for macOS (host target)..."
    cargo build -p tako-server --release
    @echo "Prebuilding tako-server for Linux targets..."
    just build::tako-server-all
    @echo "Packaging tako-server release archives..."
    @for variant in x86_64-glibc aarch64-glibc x86_64-musl aarch64-musl; do \
        tmpdir="$(mktemp -d)"; \
        cp "dist/tako-server-linux-$variant" "$tmpdir/tako-server"; \
        tar -czf "dist/tako-server-linux-$variant.tar.gz" -C "$tmpdir" tako-server; \
        rm -rf "$tmpdir"; \
    done; \
    if command -v sha256sum >/dev/null 2>&1; then \
        sha256sum dist/tako-server-linux-x86_64-glibc.tar.gz > dist/tako-server-linux-x86_64-glibc.tar.gz.sha256; \
        sha256sum dist/tako-server-linux-aarch64-glibc.tar.gz > dist/tako-server-linux-aarch64-glibc.tar.gz.sha256; \
        sha256sum dist/tako-server-linux-x86_64-musl.tar.gz > dist/tako-server-linux-x86_64-musl.tar.gz.sha256; \
        sha256sum dist/tako-server-linux-aarch64-musl.tar.gz > dist/tako-server-linux-aarch64-musl.tar.gz.sha256; \
    else \
        shasum -a 256 dist/tako-server-linux-x86_64-glibc.tar.gz > dist/tako-server-linux-x86_64-glibc.tar.gz.sha256; \
        shasum -a 256 dist/tako-server-linux-aarch64-glibc.tar.gz > dist/tako-server-linux-aarch64-glibc.tar.gz.sha256; \
        shasum -a 256 dist/tako-server-linux-x86_64-musl.tar.gz > dist/tako-server-linux-x86_64-musl.tar.gz.sha256; \
        shasum -a 256 dist/tako-server-linux-aarch64-musl.tar.gz > dist/tako-server-linux-aarch64-musl.tar.gz.sha256; \
    fi

# Generate release notes draft for `tako`.
# Output: dist/release-notes/tako.md
notes-tako series="": _guard _require-git-cliff
    @pattern='^{{ TAKO_TAG_PREFIX }}'; \
    if [ -n "{{ series }}" ]; then \
        series_prefix='{{ series }}'; \
        case "$series_prefix" in \
            *.x) series_prefix="${series_prefix%.x}." ;; \
        esac; \
        escaped_series="$(printf '%s' "$series_prefix" | sed 's/[][\.^$*+?(){}|\\]/\\&/g')"; \
        pattern="^{{ TAKO_TAG_PREFIX }}$escaped_series"; \
    fi; \
    git cliff -c cliff.toml --unreleased --tag-pattern "$pattern" \
        --include-path 'tako/**/*' \
        --include-path 'tako-core/**/*' \
        --include-path 'tako-socket/**/*' \
        --include-path 'presets/**/*' \
        --include-path 'scripts/install-tako-cli.sh' \
        -o "{{ RELEASE_NOTES_DIR }}/tako.md"

# Generate release notes draft for `tako-server`.
# Output: dist/release-notes/tako-server.md
notes-tako-server series="": _guard _require-git-cliff
    @pattern='^{{ TAKO_SERVER_TAG_PREFIX }}'; \
    if [ -n "{{ series }}" ]; then \
        series_prefix='{{ series }}'; \
        case "$series_prefix" in \
            *.x) series_prefix="${series_prefix%.x}." ;; \
        esac; \
        escaped_series="$(printf '%s' "$series_prefix" | sed 's/[][\.^$*+?(){}|\\]/\\&/g')"; \
        pattern="^{{ TAKO_SERVER_TAG_PREFIX }}$escaped_series"; \
    fi; \
    git cliff -c cliff.toml --unreleased --tag-pattern "$pattern" \
        --include-path 'tako-server/**/*' \
        --include-path 'tako-core/**/*' \
        --include-path 'tako-socket/**/*' \
        --include-path 'scripts/install-tako-server.sh' \
        -o "{{ RELEASE_NOTES_DIR }}/tako-server.md"

# Generate release notes draft for `tako.sh` (npm SDK).
# Output: dist/release-notes/sdk-npm.md
notes-sdk series="": _guard
    sh scripts/release-notes.sh \
        --component "sdk-npm" \
        --prefix "{{ SDK_TAG_PREFIX }}" \
        --series "{{ series }}" \
        --output "{{ RELEASE_NOTES_DIR }}/sdk-npm.md"

# Full `tako` release flow:
# 1) Generate notes for `tako`
# 2) Release `tako`
# 3) Publish GitHub release assets for installer downloads
tako series="": _guard
    just --justfile just/release.just notes-tako "{{ series }}"
    just --justfile just/release.just cargo-tako
    just --justfile just/release.just github-tako

# Full `tako-server` release flow:
# 1) Generate notes for `tako-server`
# 2) Release `tako-server`
# 3) Publish GitHub release assets for installer downloads
tako-server series="": _guard
    just --justfile just/release.just notes-tako-server "{{ series }}"
    just --justfile just/release.just cargo-tako-server
    just --justfile just/release.just github-tako-server

# Full `tako.sh` SDK release flow:
# 1) Generate notes for `sdk-npm`
# 2) Release `tako.sh` to npm
sdk series="": _guard
    just --justfile just/release.just notes-sdk "{{ series }}"
    just --justfile just/release.just npm

# Full `tako-core` release flow.
tako-core: _guard
    just --justfile just/release.just cargo-tako-core

# Full `tako-socket` release flow.
tako-socket: _guard
    just --justfile just/release.just cargo-tako-socket

# Publish GitHub release + assets for latest `tako` tag.
# Requires dist artifacts from release::cargo-tako prebuild.
github-tako: _guard _require-gh
    tag="$(git tag --list '{{ TAKO_TAG_PREFIX }}*' --sort=-v:refname | head -n 1 || true)"; \
    if [ -z "$tag" ]; then echo "error: no {{ TAKO_TAG_PREFIX }}* tag found" >&2; exit 1; fi; \
    if [ "$(git rev-list -n 1 "$tag")" != "$(git rev-parse HEAD)" ]; then \
        echo "error: latest tako tag ($tag) does not point to HEAD; run this after release::cargo-tako" >&2; \
        exit 1; \
    fi; \
    notes="{{ RELEASE_NOTES_DIR }}/tako.md"; \
    if [ ! -f "$notes" ]; then echo "error: missing release notes file: $notes" >&2; exit 1; fi; \
    version="${tag#{{ TAKO_TAG_PREFIX }}}"; \
    set -- \
        dist/tako-darwin-aarch64.tar.gz \
        dist/tako-darwin-aarch64.tar.gz.sha256 \
        dist/tako-darwin-x86_64.tar.gz \
        dist/tako-darwin-x86_64.tar.gz.sha256 \
        dist/tako-linux-x86_64.tar.gz \
        dist/tako-linux-x86_64.tar.gz.sha256 \
        dist/tako-linux-aarch64.tar.gz \
        dist/tako-linux-aarch64.tar.gz.sha256; \
    for asset in "$@"; do \
        if [ ! -f "$asset" ]; then \
            echo "error: missing release artifact: $asset" >&2; \
            exit 1; \
        fi; \
    done; \
    if gh release view "$tag" >/dev/null 2>&1; then \
        gh release upload "$tag" "$@" --clobber; \
        gh release edit "$tag" --title "tako v$version" --notes-file "$notes"; \
    else \
        gh release create "$tag" --title "tako v$version" --notes-file "$notes" "$@"; \
    fi

# Publish GitHub release + assets for latest `tako-server` tag.
# Requires dist artifacts from release::cargo-tako-server prebuild.
github-tako-server: _guard _require-gh
    tag="$(git tag --list '{{ TAKO_SERVER_TAG_PREFIX }}*' --sort=-v:refname | head -n 1 || true)"; \
    if [ -z "$tag" ]; then echo "error: no {{ TAKO_SERVER_TAG_PREFIX }}* tag found" >&2; exit 1; fi; \
    if [ "$(git rev-list -n 1 "$tag")" != "$(git rev-parse HEAD)" ]; then \
        echo "error: latest tako-server tag ($tag) does not point to HEAD; run this after release::cargo-tako-server" >&2; \
        exit 1; \
    fi; \
    notes="{{ RELEASE_NOTES_DIR }}/tako-server.md"; \
    if [ ! -f "$notes" ]; then echo "error: missing release notes file: $notes" >&2; exit 1; fi; \
    version="${tag#{{ TAKO_SERVER_TAG_PREFIX }}}"; \
    set -- \
        dist/tako-server-linux-x86_64-glibc.tar.gz \
        dist/tako-server-linux-x86_64-glibc.tar.gz.sha256 \
        dist/tako-server-linux-aarch64-glibc.tar.gz \
        dist/tako-server-linux-aarch64-glibc.tar.gz.sha256 \
        dist/tako-server-linux-x86_64-musl.tar.gz \
        dist/tako-server-linux-x86_64-musl.tar.gz.sha256 \
        dist/tako-server-linux-aarch64-musl.tar.gz \
        dist/tako-server-linux-aarch64-musl.tar.gz.sha256; \
    for asset in "$@"; do \
        if [ ! -f "$asset" ]; then \
            echo "error: missing release artifact: $asset" >&2; \
            exit 1; \
        fi; \
    done; \
    if gh release view "$tag" >/dev/null 2>&1; then \
        gh release upload "$tag" "$@" --clobber; \
        gh release edit "$tag" --title "tako-server v$version" --notes-file "$notes"; \
    else \
        gh release create "$tag" --title "tako-server v$version" --notes-file "$notes" "$@"; \
    fi

# Release and publish `tako-core`:
# - prompts next version
# - updates tako-core/Cargo.toml (plus dependent workspace manifests/lockfile as needed)
# - commits, tags, pushes
# - runs `cargo publish -p tako-core`
cargo-tako-core: _guard _require-cargo-set-version
    current="$(awk -F'\"' '/^version = \"/ { print $2; exit }' tako-core/Cargo.toml)"; \
    echo "Current tako-core version: $current"; \
    printf "Next tako-core version: "; \
    read -r next; \
    if [ -z "$next" ]; then echo "error: version is required" >&2; exit 1; fi; \
    cargo set-version --package tako-core "$next"; \
    if [ -z "$(git diff -- tako-core/Cargo.toml)" ]; then echo "error: version did not change" >&2; exit 1; fi; \
    unexpected="$(git diff --name-only | grep -Ev '^(Cargo.lock|tako-core/Cargo.toml|tako/Cargo.toml|tako-server/Cargo.toml)$' || true)"; \
    if [ -n "$unexpected" ]; then \
        echo "error: unexpected file changes before tako-core release commit" >&2; \
        echo "$unexpected" >&2; \
        exit 1; \
    fi; \
    git add Cargo.lock tako-core/Cargo.toml tako/Cargo.toml tako-server/Cargo.toml; \
    staged_unexpected="$(git diff --cached --name-only | grep -Ev '^(Cargo.lock|tako-core/Cargo.toml|tako/Cargo.toml|tako-server/Cargo.toml)$' || true)"; \
    if [ -n "$staged_unexpected" ]; then \
        echo "error: unexpected staged files before tako-core release commit" >&2; \
        echo "$staged_unexpected" >&2; \
        exit 1; \
    fi; \
    if ! git diff --cached --name-only | grep -qx 'tako-core/Cargo.toml'; then \
        echo "error: expected tako-core/Cargo.toml to be staged" >&2; \
        git diff --cached --name-only >&2; \
        exit 1; \
    fi; \
    tag="{{ TAKO_CORE_TAG_PREFIX }}$next"; \
    if git rev-parse --verify --quiet "refs/tags/$tag" >/dev/null; then \
        echo "error: tag '$tag' already exists" >&2; \
        exit 1; \
    fi; \
    git commit -m "chore(release): tako-core v$next"; \
    if ! git tag -s "$tag" -m "tako-core v$next"; then \
        echo "error: failed to create signed tag '$tag'; configure git tag signing and retry" >&2; \
        exit 1; \
    fi; \
    git push origin master --follow-tags
    @just --justfile just/release.just _guard
    cargo publish -p tako-core

# Release and publish `tako-socket`:
# - prompts next version
# - updates tako-socket/Cargo.toml (plus dependent workspace manifests/lockfile as needed)
# - commits, tags, pushes
# - runs `cargo publish -p tako-socket`
cargo-tako-socket: _guard _require-cargo-set-version
    current="$(awk -F'\"' '/^version = \"/ { print $2; exit }' tako-socket/Cargo.toml)"; \
    echo "Current tako-socket version: $current"; \
    printf "Next tako-socket version: "; \
    read -r next; \
    if [ -z "$next" ]; then echo "error: version is required" >&2; exit 1; fi; \
    cargo set-version --package tako-socket "$next"; \
    if [ -z "$(git diff -- tako-socket/Cargo.toml)" ]; then echo "error: version did not change" >&2; exit 1; fi; \
    unexpected="$(git diff --name-only | grep -Ev '^(Cargo.lock|tako-socket/Cargo.toml|tako/Cargo.toml|tako-server/Cargo.toml)$' || true)"; \
    if [ -n "$unexpected" ]; then \
        echo "error: unexpected file changes before tako-socket release commit" >&2; \
        echo "$unexpected" >&2; \
        exit 1; \
    fi; \
    git add Cargo.lock tako-socket/Cargo.toml tako/Cargo.toml tako-server/Cargo.toml; \
    staged_unexpected="$(git diff --cached --name-only | grep -Ev '^(Cargo.lock|tako-socket/Cargo.toml|tako/Cargo.toml|tako-server/Cargo.toml)$' || true)"; \
    if [ -n "$staged_unexpected" ]; then \
        echo "error: unexpected staged files before tako-socket release commit" >&2; \
        echo "$staged_unexpected" >&2; \
        exit 1; \
    fi; \
    if ! git diff --cached --name-only | grep -qx 'tako-socket/Cargo.toml'; then \
        echo "error: expected tako-socket/Cargo.toml to be staged" >&2; \
        git diff --cached --name-only >&2; \
        exit 1; \
    fi; \
    tag="{{ TAKO_SOCKET_TAG_PREFIX }}$next"; \
    if git rev-parse --verify --quiet "refs/tags/$tag" >/dev/null; then \
        echo "error: tag '$tag' already exists" >&2; \
        exit 1; \
    fi; \
    git commit -m "chore(release): tako-socket v$next"; \
    if ! git tag -s "$tag" -m "tako-socket v$next"; then \
        echo "error: failed to create signed tag '$tag'; configure git tag signing and retry" >&2; \
        exit 1; \
    fi; \
    git push origin master --follow-tags
    @just --justfile just/release.just _guard
    cargo publish -p tako-socket

# Release and publish `tako`:
# - prompts next version
# - updates tako/Cargo.toml (and Cargo.lock)
# - commits, tags, pushes
# - runs `cargo publish -p tako`
cargo-tako: _guard _guard-shared-crates-released _require-cargo-set-version _prebuild-tako
    current="$(awk -F'\"' '/^version = \"/ { print $2; exit }' tako/Cargo.toml)"; \
    echo "Current tako version: $current"; \
    printf "Next tako version: "; \
    read -r next; \
    if [ -z "$next" ]; then echo "error: version is required" >&2; exit 1; fi; \
    cargo set-version --package tako "$next"; \
    if [ -z "$(git diff -- tako/Cargo.toml)" ]; then echo "error: version did not change" >&2; exit 1; fi; \
    unexpected="$(git diff --name-only | grep -Ev '^(Cargo.lock|tako/Cargo.toml)$' || true)"; \
    if [ -n "$unexpected" ]; then \
        echo "error: unexpected file changes before tako release commit" >&2; \
        echo "$unexpected" >&2; \
        exit 1; \
    fi; \
    git add Cargo.lock tako/Cargo.toml; \
    staged_unexpected="$(git diff --cached --name-only | grep -Ev '^(Cargo.lock|tako/Cargo.toml)$' || true)"; \
    if [ -n "$staged_unexpected" ]; then \
        echo "error: unexpected staged files before tako release commit" >&2; \
        echo "$staged_unexpected" >&2; \
        exit 1; \
    fi; \
    if ! git diff --cached --name-only | grep -qx 'tako/Cargo.toml'; then \
        echo "error: expected tako/Cargo.toml to be staged" >&2; \
        git diff --cached --name-only >&2; \
        exit 1; \
    fi; \
    tag="{{ TAKO_TAG_PREFIX }}$next"; \
    if git rev-parse --verify --quiet "refs/tags/$tag" >/dev/null; then \
        echo "error: tag '$tag' already exists" >&2; \
        exit 1; \
    fi; \
    git commit -m "chore(release): tako v$next"; \
    if ! git tag -s "$tag" -m "tako v$next"; then \
        echo "error: failed to create signed tag '$tag'; configure git tag signing and retry" >&2; \
        exit 1; \
    fi; \
    git push origin master --follow-tags
    @just --justfile just/release.just _guard
    cargo publish -p tako

# Release and publish `tako-server`:
# - prompts next version
# - updates tako-server/Cargo.toml (and Cargo.lock)
# - commits, tags, pushes
# - runs `cargo publish -p tako-server`
cargo-tako-server: _guard _guard-shared-crates-released _require-cargo-set-version _prebuild-tako-server
    current="$(awk -F'\"' '/^version = \"/ { print $2; exit }' tako-server/Cargo.toml)"; \
    echo "Current tako-server version: $current"; \
    printf "Next tako-server version: "; \
    read -r next; \
    if [ -z "$next" ]; then echo "error: version is required" >&2; exit 1; fi; \
    cargo set-version --package tako-server "$next"; \
    if [ -z "$(git diff -- tako-server/Cargo.toml)" ]; then echo "error: version did not change" >&2; exit 1; fi; \
    unexpected="$(git diff --name-only | grep -Ev '^(Cargo.lock|tako-server/Cargo.toml)$' || true)"; \
    if [ -n "$unexpected" ]; then \
        echo "error: unexpected file changes before tako-server release commit" >&2; \
        echo "$unexpected" >&2; \
        exit 1; \
    fi; \
    git add Cargo.lock tako-server/Cargo.toml; \
    staged_unexpected="$(git diff --cached --name-only | grep -Ev '^(Cargo.lock|tako-server/Cargo.toml)$' || true)"; \
    if [ -n "$staged_unexpected" ]; then \
        echo "error: unexpected staged files before tako-server release commit" >&2; \
        echo "$staged_unexpected" >&2; \
        exit 1; \
    fi; \
    if ! git diff --cached --name-only | grep -qx 'tako-server/Cargo.toml'; then \
        echo "error: expected tako-server/Cargo.toml to be staged" >&2; \
        git diff --cached --name-only >&2; \
        exit 1; \
    fi; \
    tag="{{ TAKO_SERVER_TAG_PREFIX }}$next"; \
    if git rev-parse --verify --quiet "refs/tags/$tag" >/dev/null; then \
        echo "error: tag '$tag' already exists" >&2; \
        exit 1; \
    fi; \
    git commit -m "chore(release): tako-server v$next"; \
    if ! git tag -s "$tag" -m "tako-server v$next"; then \
        echo "error: failed to create signed tag '$tag'; configure git tag signing and retry" >&2; \
        exit 1; \
    fi; \
    git push origin master --follow-tags
    @just --justfile just/release.just _guard
    cargo publish -p tako-server

# Release and publish npm SDK (`tako.sh`):
# - prompts next version
# - updates sdk/js/package.json
# - commits, tags, pushes
# - runs `npm publish` in sdk/js/
npm: _guard
    current="$(cd sdk/js && npm pkg get version --json | tr -d '\"')"; \
    echo "Current sdk version: $current"; \
    printf "Next sdk version: "; \
    read -r next; \
    if [ -z "$next" ]; then echo "error: version is required" >&2; exit 1; fi; \
    (cd sdk/js && npm pkg set version="$next"); \
    if [ -z "$(git diff -- sdk/js/package.json)" ]; then echo "error: version did not change" >&2; exit 1; fi; \
    unexpected="$(git diff --name-only | grep -v '^sdk/js/package.json$' || true)"; \
    if [ -n "$unexpected" ]; then \
        echo "error: expected only sdk/js/package.json to change before release commit" >&2; \
        echo "$unexpected" >&2; \
        exit 1; \
    fi; \
    git add sdk/js/package.json; \
    if [ "$(git diff --cached --name-only)" != "sdk/js/package.json" ]; then \
        echo "error: expected only sdk/js/package.json to be staged" >&2; \
        git diff --cached --name-only >&2; \
        exit 1; \
    fi; \
    tag="{{ SDK_TAG_PREFIX }}$next"; \
    if git rev-parse --verify --quiet "refs/tags/$tag" >/dev/null; then \
        echo "error: tag '$tag' already exists" >&2; \
        exit 1; \
    fi; \
    git commit -m "chore(release): sdk v$next"; \
    if ! git tag -s "$tag" -m "tako.sh v$next"; then \
        echo "error: failed to create signed tag '$tag'; configure git tag signing and retry" >&2; \
        exit 1; \
    fi; \
    git push origin master --follow-tags
    @just --justfile just/release.just _guard
    cd sdk/js && npm publish

# Publish multi-arch builder images to container registry.
# Usage: just release builder-images <version> [registry]
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
