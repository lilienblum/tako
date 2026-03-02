mod build 'just/build.just'
mod testbed 'just/testbed.just'
mod release 'just/release.just'
mod test 'just/test.just'

export TAKO_HOME := "local-dev/.tako"

tako *arguments:
    cargo build -p tako --release
    TAKO_HOME="$(pwd)/{{ TAKO_HOME }}" ./target/release/tako {{ arguments }}

fmt:
    cargo fmt
    bun run fmt

lint:
    cargo clippy --workspace --all-targets
    bun run --filter '*' typecheck

ci: fmt lint test::all

e2e fixture="e2e/fixtures/js/tanstack-start": (test::e2e fixture)
