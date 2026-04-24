---
layout: ../../layouts/DocsLayout.astro
title: "Framework presets for Next.js, TanStack Start, and more - Tako Docs"
heading: Presets
current: presets
description: "Learn how Tako presets provide framework-specific defaults for entrypoints, static assets, and dev commands across supported frameworks."
---

# Presets

Presets are metadata-only defaults that tell Tako how to run a framework app. A preset can set an entrypoint (`main`), static asset directories (`assets`), and a dev command (`dev`). That's the entire surface area — presets never carry build, install, or start commands.

Every app uses a preset. If you don't name one, Tako falls back to the base runtime adapter for your selected runtime.

## Base runtime adapters

The `bun`, `node`, `deno`, and `go` adapters ship compiled into the CLI as runtime plugins (see `tako-runtime/src/plugins/`). They own runtime concerns — install commands, launch arguments, package-manager detection — and also supply a default entrypoint when no framework preset overrides it.

| Runtime | Default `main` |
| ------- | -------------- |
| `bun`   | `src/index.ts` |
| `node`  | `index.js`     |
| `deno`  | `main.ts`      |
| `go`    | `app`          |

Base adapters are not presets. Presets layer on top of an adapter and only contribute metadata.

## Built-in framework presets

These are the official presets shipped in `tako-sh/presets`. Their family manifest (`presets/javascript.toml`) is also embedded in the CLI so `tako dev` can resolve them offline on first run.

### tanstack-start

```toml
[tanstack-start]
main = "dist/server/tako-entry.mjs"
assets = ["dist/client"]
dev = ["vite", "dev"]
```

Pairs with the `tako.sh/vite` plugin, which emits `dist/server/tako-entry.mjs` during `vite build` to wrap the SSR bundle with Tako's request handling. `dist/client` is merged into `public/` after build, and `tako dev` runs `vite dev`.

### nextjs

```toml
[nextjs]
main = ".next/tako-entry.mjs"
dev = ["next", "dev"]
```

Uses the `tako.sh/nextjs` adapter, which writes `.next/tako-entry.mjs` after `next build`. If Next emits standalone output the wrapper takes the fast path; otherwise it falls back to `next start`. `tako dev` runs `next dev`.

### vite

```toml
[vite]
dev = ["vite", "dev"]
```

Just a dev command. No `main`, no `assets` — use this when you want Tako to drive `vite dev` but take `main` from `package.json` or the base adapter.

## Selecting a preset

Set the `runtime` and `preset` fields in `tako.toml`:

```toml
runtime = "bun"
preset = "tanstack-start"
```

`runtime` picks the base adapter; `preset` names the framework preset resolved under that runtime. A few rules:

- Use the short alias (`tanstack-start`), not a namespaced form like `js/tanstack-start`.
- `github:` references are not accepted.
- Pin a specific commit with `@<commit-hash>` (see [Pinning a preset version](#pinning-a-preset-version)).
- Omit `preset` entirely to stay on the base adapter.

## What presets contain

Every preset supports:

- **`name`** — Optional display label. Defaults to the TOML section name.
- **`main`** — Default app entrypoint.
- **`assets`** — Directories merged into `public/` after build.
- **`dev`** — Command used by `tako dev`.

What presets do **not** contain: build commands, install commands, or start commands. Build steps belong in `[build]` / `[[build_stages]]` in your `tako.toml`. Install and launch behavior lives in runtime plugins.

## How presets merge with tako.toml

Your `tako.toml` always wins over preset defaults.

**Entrypoint (`main`) resolution order**

1. `main` in `tako.toml`
2. `main` from `package.json`
3. Preset `main`

For JS runtimes, if the preset `main` is an index file (`index.ts`, `index.js`, `src/index.ts`, etc.), Tako first checks whether the project-level `index.*` or `src/index.*` exists and prefers that. If no source produces a `main`, dev and deploy fail with guidance.

**Assets**

Preset `assets` are unioned with top-level `assets` from `tako.toml` (deduplicated) and merged into `public/` in order, so later entries overwrite earlier ones.

**Dev command priority**

1. `dev` in `tako.toml`
2. Preset `dev`
3. Runtime default (JS runtimes boot the SDK dev entrypoint; Go uses `go run .`)

## Preset definition files

Official presets are grouped into family manifests at `presets/<language>.toml`. Each alias is a TOML section:

```toml
# presets/javascript.toml

[vite]
dev = ["vite", "dev"]

[tanstack-start]
main = "dist/server/tako-entry.mjs"
assets = ["dist/client"]
dev = ["vite", "dev"]

[nextjs]
main = ".next/tako-entry.mjs"
dev = ["next", "dev"]
```

### Runtime-local overrides

A preset can declare runtime-specific nested sections as `[<preset>.<runtime>]`. When the selected runtime matches, its `dev` replaces the base preset's `dev`. Only `dev` is overridable — `main`, `assets`, and `name` always come from the base section.

```toml
# presets/javascript.toml

[vite]
dev = ["vite", "dev"]

[vite.bun]
# Run Vite's resolved bin directly under `bun --bun` so the SSR graph
# uses Bun's ESM loader. `bunx --bun` goes through a shim that drops
# fds > 2, which breaks Tako's fd-4 readiness handshake.
dev = ["bun", "--bun", "./node_modules/.bin/vite", "dev"]
```

This is how the built-in JavaScript presets handle Bun transparently, without asking users to memorize Bun's invocation quirks.

## Preset resolution

Official definitions live in the `tako-sh/presets` GitHub repo. Tako caches fetched branch manifests locally and keeps them usable offline.

- **`tako dev`** — Prefers cached or embedded preset data; only fetches from `master` when nothing local is available. Startup doesn't block on GitHub when a cached or embedded manifest exists.
- **`tako deploy`** — Refreshes unpinned aliases (e.g. `preset = "tanstack-start"`) from the `master` branch. Branch manifests are cached for roughly one hour. If the fetch fails, Tako falls back to previously cached content.
- **First run** — Embedded JS and Go family manifests let `tako dev` boot offline. `tako deploy` needs a successful fetch the first time unless the preset is already cached.
- **Alternate repo** — Point `PACKAGE_REPOSITORY_URL` at a fork when developing new presets locally.

### Pinning a preset version

Append `@<commit-hash>` to lock a preset to a specific commit:

```toml
preset = "tanstack-start@a1b2c3d"
```

Pinned presets are fetched from that exact commit, so upstream changes never affect your builds.

## Presets during `tako init`

`tako init` fetches the runtime-family preset manifest (for example `presets/javascript.toml`) and shows a selector with the available aliases. While loading, it displays `Fetching presets...`.

- If no family presets are available after the fetch, init skips preset selection and uses the base adapter.
- `preset` is only written to `tako.toml` when you select a non-base preset. Base adapters and the custom preset-reference option leave `preset` commented out.
- `main` is only prompted when neither adapter inference nor the preset default provides one. If inference produces the same `main` the preset would give, `main` is omitted from the written config.
