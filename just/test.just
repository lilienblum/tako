set working-directory := ".."

default: all

all: workspace sdk cli e2e

workspace:
    cargo test --workspace

sdk:
    bun run --filter 'tako.sh' build
    bun run --filter 'tako.sh' typecheck
    bun test --cwd sdk/javascript

cli:
    cargo build -p tako --bin tako
    TAKO_BIN="$(pwd)/target/debug/tako" bun test --cwd e2e/cli

e2e fixture="":
    #!/usr/bin/env bash
    set -euo pipefail
    if [ -n "{{ fixture }}" ]; then
        ./e2e/run.sh "{{ fixture }}"
    else
        for fixture in $(find e2e/fixtures/javascript -mindepth 1 -maxdepth 1 -type d | sort); do
            ./e2e/run.sh "$fixture"
        done
    fi
