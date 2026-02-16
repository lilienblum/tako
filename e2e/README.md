# E2E Fixtures

End-to-end fixture apps and assets used by integration tests.

## Run Docker E2E Fixtures

From repo root:

```bash
just e2e e2e/fixtures/js/bun
just e2e e2e/fixtures/js/tanstack-start
```

This runs the global e2e harness in `e2e/run.sh` against the fixture path.
The harness generates an ephemeral SSH keypair per run inside a disposable Docker volume, starts real `tako-server` binaries on Ubuntu + AlmaLinux test hosts, and never uses `~/.ssh`.
Rust build caches are stored outside the repo at `${XDG_CACHE_HOME:-~/.cache}/tako/e2e` by default:

- `cargo-home` for Cargo registry/git cache
- `target` for build outputs

Override cache locations with:

```bash
E2E_CARGO_HOME_DIR=/path/to/cargo-home E2E_CARGO_TARGET_DIR=/path/to/target ./e2e/run.sh e2e/fixtures/js/tanstack-start
```

After deploy, it runs universal runtime checks:

- App health endpoint responds with valid JSON.
- App root responds with valid HTML or JSON.
- Static/public files (if present in release) are fetched over HTTP.
- Compiled static assets (if present or referenced by HTML) are fetched over HTTP.
