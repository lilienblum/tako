set working-directory := ".."

default: workspace

workspace:
    cargo test --workspace

sdk:
    cd sdk && bun test

e2e fixture="e2e/js/tanstack-start":
    ./e2e/run.sh {{ fixture }}
