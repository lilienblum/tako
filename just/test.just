set working-directory := ".."

default: all

all: workspace sdk cli e2e

workspace:
    cargo test --workspace

sdk:
    bun run --filter 'tako.sh' build
    bun run --filter 'tako.sh' typecheck
    bun test --cwd sdk/js

cli:
    cargo build -p tako --bin tako
    bun test --cwd e2e/cli

e2e fixture="":
    #!/usr/bin/env bash
    set -euo pipefail
    if [ -n "{{ fixture }}" ]; then
        ./e2e/run.sh "{{ fixture }}"
    else
        ./e2e/run.sh e2e/fixtures/js/bun
        ./e2e/run.sh e2e/fixtures/js/tanstack-start
    fi
