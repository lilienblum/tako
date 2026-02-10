# Tako Website

Astro static site served by a Cloudflare Worker.

## Routes

- `/`: static landing page
- `/docs`: docs hub
- `/docs/install`: installation and installer script docs
- `/docs/spec`: rendered specification
- `/docs/development`: local development guide
- `/docs/deployment`: deployment guide
- `/docs/operations`: operations runbook
- `/docs/architecture`: architecture overview
- `/install`: POSIX `sh` installer script for `tako` CLI (`text/plain`)
- `/install-server`: POSIX `sh` installer script for `tako-server` (`text/plain`)
- `/server-install`: alias for `/install-server`

## Run Locally

```bash
bun install
bun run --cwd website dev
```

## Test Installer Endpoints Locally

```bash
curl -fsSL http://localhost:4321/install | sh
curl -fsSL http://localhost:4321/install-server | sh
```

## Build and Deploy

```bash
bun run --cwd website build
bun run --cwd website deploy
```
