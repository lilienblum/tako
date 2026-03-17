# Backlog

## Runtime management

- **Use proto_core as a library** — Replace shell-out to `proto` CLI in `version_manager.rs` with direct Rust API calls via [proto_core](https://crates.io/crates/proto_core). Eliminates PATH issues, proto CLI dependency on servers, and subprocess overhead.

- **Install script proto install fails in Docker build** — `sudo -u tako` works for real installs but proto's temp file execution fails in Docker build. E2E Dockerfiles work around this with `TAKO_INSTALL_PROTO=0` + `USER tako` directive.

## E2E

- **Docker layer caching** — Server container builds (proto install, apt-get) are not cached between CI runs. `docker compose build` doesn't support BuildKit cache env vars. Would need `docker buildx bake` or `compose.yml` cache directives.
