set working-directory := ".."

default: workspace

workspace:
    cargo test --workspace

sdk:
    cd sdk && bun test
