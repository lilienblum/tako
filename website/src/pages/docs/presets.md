---
layout: ../../layouts/DocsLayout.astro
title: "Framework presets for Next.js, TanStack Start, and more - Tako Docs"
heading: Presets
current: presets
description: "Learn how Tako presets provide framework-specific defaults for entrypoints, static assets, and dev commands across supported frameworks."
---

# Presets

Presets provide metadata defaults for deploying framework apps without extra configuration. A preset can define a default entrypoint (`main`), static asset directories (`assets`), and a custom dev command (`dev`). Presets are metadata-only -- they never contain build commands, install commands, or start commands.

Every app uses a preset. If you do not set one explicitly, Tako uses the base adapter for your detected runtime.

## Base runtime adapters

Tako ships with four built-in base adapters: `bun`, `node`, `deno`, and `go`. These are compiled into the CLI and serve as the default when no framework preset is selected.

Each base adapter defines a default entrypoint:

| Adapter | Default `main` |
| ------- | -------------- |
| `bun`   | `src/index.ts` |
| `node`  | `index.js`     |
| `deno`  | `main.ts`      |
| `go`    | `app`          |

Base adapters are not the same as presets. They are runtime plugins (`tako-runtime/src/plugins/`) that handle runtime behavior: install commands, launch arguments, entrypoint resolution, and package manager detection. Presets sit on top of adapters and only add metadata.

## Built-in presets

Tako includes official framework presets that provide the right `main`, `assets`, and `dev` values for supported frameworks.

### tanstack-start

```toml
[tanstack-start]
main = "dist/server/tako-entry.mjs"
assets = ["dist/client"]
dev = ["vite", "dev"]
```

Launches the SSR bundle emitted by `tako.sh/vite` after `vite build`, which wraps the TanStack Start server with Tako endpoint handling. Marks `dist/client` as the asset directory to merge into `public/` after build, and uses `vite dev` for local development.

### nextjs

```toml
[nextjs]
main = ".next/tako-entry.mjs"
dev = ["next", "dev"]
```

Uses the `tako.sh/nextjs` adapter output. The adapter generates `.next/tako-entry.mjs` after `next build`, and `tako dev` runs `next dev`. If Next emits standalone output, Tako uses it; otherwise the wrapper falls back to `next start`.

### vite

```toml
[vite]
dev = ["vite", "dev"]
```

For projects using Vite as their dev server. This preset only sets the dev command -- it does not define `main` or `assets`, so those come from your `tako.toml` or the base adapter.

## Setting a preset

Set a preset in `tako.toml` with the `preset` field:

```toml
runtime = "bun"
preset = "tanstack-start"
```

The `runtime` field selects which base adapter to use. The `preset` field names the framework preset to layer on top. Use the short name only -- namespaced syntax like `js/tanstack-start` is rejected. `github:` references are also not supported.

If you omit `preset`, Tako uses the base adapter for your runtime with no framework-specific metadata.

## Preset fields

Each preset definition supports these fields:

- **`name`** (optional) -- Display name. Falls back to the TOML section name.
- **`main`** (optional) -- Default app entrypoint. Your `tako.toml` `main` takes precedence if set.
- **`assets`** (optional) -- List of directories to merge into `public/` after build.
- **`dev`** (optional) -- Custom dev command for `tako dev`. Framework presets use this to run their own dev server (e.g. `vite dev`) instead of the SDK entrypoint.

Presets do **not** contain build commands, install commands, or start commands. All build configuration belongs in your `tako.toml` under `[build]` or `[[build_stages]]`. All runtime behavior (install, launch, entrypoint resolution) lives in runtime plugins.

## How presets merge with tako.toml

Presets provide defaults. Your `tako.toml` settings always take precedence.

**Entrypoint (`main`):** Resolution order is: `main` in `tako.toml` > `main` in `package.json` > preset `main`. For JS runtimes, when the preset `main` is an index file like `index.ts` or `src/index.ts`, Tako checks whether the file exists in your project before using the preset value. If no source provides `main`, deploy and dev fail with guidance.

**Assets:** Preset `assets` are combined with your top-level `assets` in `tako.toml` (deduplicated). Assets are merged into `public/` after the build, with later entries overwriting earlier ones.

**Dev command:** `tako dev` resolves the dev command with this priority:

1. `dev` in `tako.toml` (user override, e.g. `dev = ["custom", "cmd"]`)
2. Preset `dev` command (e.g. the vite preset uses `vite dev`)
3. Runtime default: JS runtimes run through the SDK entrypoint, Go uses `go run .`

## Preset definition files

Official preset definitions are organized by language in family files at `presets/<language>.toml`. For example:

```
presets/javascript.toml
presets/go.toml
```

Each file contains framework presets as TOML sections, where each section name is the preset alias:

```toml
# presets/javascript.toml

[vite]
dev = ["vite", "dev"]

[tanstack-start]
main = "dist/server/tako-entry.mjs"
assets = ["dist/client"]
dev = ["vite", "dev"]
```

### Runtime-local overrides

A preset can declare runtime-specific overrides as nested sections (`[<preset>.<runtime>]`). When the selected runtime matches one of these sections, those fields replace the corresponding base-preset fields; any field the override leaves out falls through to the base section.

```toml
# presets/javascript.toml

[vite]
dev = ["vite", "dev"]

[vite.bun]
# Run Vite's script directly via `bun --bun` so the SSR graph uses Bun's ESM
# loader. `bunx --bun` drops fds > 2, which breaks Tako's fd-4 readiness
# handshake, so we target the resolved bin script.
dev = ["bun", "--bun", "./node_modules/.bin/vite", "dev"]
```

This is how the built-in JavaScript presets adapt to Bun without forcing every user to memorize runtime-specific invocation quirks.

## Preset resolution

When you deploy or run `tako dev`, Tako resolves your preset alias into actual metadata. Presets are cached locally for offline use.

**`tako dev`** prefers cached preset data immediately. If you already have a cached or embedded preset manifest, dev startup does not wait on GitHub. It only fetches from the `master` branch when nothing local is available.

**`tako deploy`** refreshes unpinned aliases (for example `preset = "tanstack-start"`) from the `master` branch of the presets repository. Fresh branch metadata is cached for 1 hour. If the refresh fails, Tako falls back to previously cached content.

**First run**: `tako dev` can start offline for built-in presets because JS and Go family manifests are embedded in the CLI. `tako deploy` still needs a successful fetch the first time unless the preset is already cached.

### Pinning a preset version

To lock a preset to a specific commit and guarantee reproducible builds, append `@<commit-hash>`:

```toml
preset = "tanstack-start@a1b2c3d"
```

Pinned presets fetch from that exact commit instead of `master`, so upstream changes never affect your builds.

## Presets during tako init

When you run `tako init` interactively, Tako fetches the runtime-family preset manifest (e.g. `presets/javascript.toml`) and shows a selector with the available presets for your detected runtime. While loading, it displays `Fetching presets...`.

- If no family presets are available after fetch, init skips preset selection and uses the base adapter.
- When a non-base preset is selected, init writes `preset` to `tako.toml`. For base adapters and the "custom preset reference" option, `preset` is left commented out.
- For `main`, init checks whether the adapter can infer an entrypoint. If the inferred `main` differs from the preset default, it is written to `tako.toml`. If they match (or the preset provides a default and no inference is available), `main` is omitted. Init only prompts for `main` when neither inference nor preset default is available.
