# Tako Website

Astro static site deployed with Cloudflare Workers static assets.

## Routes

- `/`: static landing page
- `/docs`: docs intro page with docs navigation sidebar (mobile hamburger menu)
- `/docs/quickstart`: user quickstart (local setup + remote setup)
- `/docs/install`: installer script reference
- `/docs/cli`: CLI command reference
- `/docs/tako-toml`: `tako.toml` configuration reference
- `/docs/development`: local development guide
- `/docs/deployment`: deployment guide
- `/docs/operations`: operations runbook
- `/docs/architecture`: architecture overview
- `/install`: `302` redirect to GitHub-hosted POSIX `sh` installer script for `tako`
- `/install-server`: `302` redirect to GitHub-hosted POSIX `sh` installer script for `tako-server`
- `/server-install`: alias for `/install-server` (same redirect target)

Installer redirects are configured in `public/_redirects` (Cloudflare static assets redirects).

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
