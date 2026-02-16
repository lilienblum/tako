set working-directory := ".."

default: all

all: workspace sdk e2e-all

workspace:
    cargo test --workspace

sdk:
    cd sdk && bun run build
    cd sdk && bun run typecheck
    cd sdk && bun test

e2e-all:
    ./e2e/run.sh e2e/fixtures/js/bun
    ./e2e/run.sh e2e/fixtures/js/tanstack-start

e2e fixture="e2e/fixtures/js/tanstack-start":
    ./e2e/run.sh {{ fixture }}
