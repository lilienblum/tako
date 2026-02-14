# Tako Website

Astro static site deployed with Cloudflare Workers static assets.

## Routes

- `/`: static landing page
- `/docs`: docs intro alias (redirects to `/docs/intro`)
- `/docs/intro`: docs Intro page ("The Why" section first) with docs navigation sidebar (mobile hamburger menu)
- `/docs/the-why`: compatibility alias (redirects to `/docs/intro`)
- `/docs/quickstart`: user quickstart (local setup + remote setup)
- `/docs/framework-guides`: framework adapter examples
- `/docs/cli-reference`: CLI command reference
- `/docs/cli`: compatibility alias (redirects to `/docs/cli-reference`)
- `/docs/tako-toml-reference`: `tako.toml` configuration reference
- `/docs/tako-toml`: compatibility alias (redirects to `/docs/tako-toml-reference`)
- `/docs/development`: local development guide
- `/docs/deployment`: deployment guide
- `/docs/troubleshooting`: troubleshooting runbook
- `/docs/operations`: compatibility alias (redirects to `/docs/troubleshooting`)
- `/docs/how-tako-works`: how Tako works overview
- `/docs/architecture`: compatibility alias (redirects to `/docs/how-tako-works`)
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
