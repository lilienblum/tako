set working-directory := ".."

default: all

all: workspace sdk e2e-all

workspace:
    cargo test --workspace

sdk:
    cd sdk/js && bun run build
    cd sdk/js && bun run typecheck
    cd sdk/js && bun test

e2e-all:
    ./e2e/run.sh e2e/fixtures/js/bun
    ./e2e/run.sh e2e/fixtures/js/tanstack-start

e2e fixture="e2e/fixtures/js/tanstack-start":
    ./e2e/run.sh {{ fixture }}
