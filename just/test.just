set working-directory := ".."

default: all

all: workspace sdk sdk-go cli e2e

workspace:
    cargo test --workspace

sdk:
    bun run --filter 'tako.sh' build
    bun run --filter 'tako.sh' typecheck
    bun test --cwd sdk/javascript

sdk-go:
    cd sdk/go && go test ./...
    for dir in examples/go/*/; do echo "Building $dir..." && (cd "$dir" && go build -o /dev/null .); done

cli:
    cargo build -p tako --bin tako
    TAKO_BIN="$(pwd)/target/debug/tako" bun test --cwd e2e/cli

e2e fixture="":
    #!/usr/bin/env bash
    set -euo pipefail
    if [ -n "{{ fixture }}" ]; then
        ./e2e/run.sh "{{ fixture }}"
    else
        for fixture in $(find e2e/fixtures -mindepth 2 -maxdepth 2 -type d | sort); do
            ./e2e/run.sh "$fixture"
        done
    fi
