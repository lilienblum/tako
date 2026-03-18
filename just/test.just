set working-directory := ".."

default: all

all: workspace sdk cli e2e

workspace:
    cargo test --workspace

sdk:
    cd sdk/js && bun run build
    cd sdk/js && bun run typecheck
    cd sdk/js && bun test

cli:
    cd e2e/cli && bun test

e2e fixture="":
    #!/usr/bin/env bash
    set -euo pipefail
    if [ -n "{{ fixture }}" ]; then
        ./e2e/run.sh "{{ fixture }}"
    else
        ./e2e/run.sh e2e/fixtures/js/bun
        ./e2e/run.sh e2e/fixtures/js/tanstack-start
    fi
