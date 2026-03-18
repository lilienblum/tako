# Backlog

## E2E

- **Docker layer caching** — Server container builds (apt-get) are not cached between CI runs. `docker compose build` doesn't support BuildKit cache env vars. Would need `docker buildx bake` or `compose.yml` cache directives.
