mod build 'just/build.just'
mod testbed 'just/testbed.just'
mod release 'just/release.just'
mod test 'just/test.just'

export TAKO_HOME := "local-dev/.tako"

tako *arguments:
    TAKO_HOME="$(pwd)/{{ TAKO_HOME }}" cargo run -p tako --bin tako --release -- {{ arguments }}

fmt:
    cargo fmt
    bun run fmt

lint:
    cargo clippy --workspace --all-targets
    cd sdk && bun run typecheck

ci: fmt lint test::all

e2e fixture="e2e/fixtures/js/tanstack-start": (test::e2e fixture)
