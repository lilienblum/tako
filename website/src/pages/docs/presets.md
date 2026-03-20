---
layout: ../../layouts/DocsLayout.astro
title: "Tako Docs - Presets"
heading: Presets
current: presets
---

# Presets

Presets provide metadata defaults -- specifically `main` (entrypoint) and `assets` (static asset directories) -- so you can deploy framework apps without extra config. Presets are metadata-only: they do not contain build, install, start, or dev commands.

Every app uses a preset. If you do not set one explicitly, Tako picks the **base preset** for your detected runtime.

## Base presets

Tako ships with three built-in runtime base presets: `bun`, `node`, and `deno`. These are compiled into the CLI -- they are never loaded from files on disk.

Each base preset defines a default entrypoint for its runtime:

| Field  | Bun            | Node       | Deno      |
| ------ | -------------- | ---------- | --------- |
| `main` | `src/index.ts` | `index.js` | `main.ts` |

## Official presets

Official presets provide framework-specific `main` and `assets` defaults on top of a base preset. For example, `tanstack-start` knows the right entrypoint and asset directory for TanStack Start apps.

Set a preset in `tako.toml`:

```toml
runtime = "bun"
preset = "tanstack-start"
```

The `runtime` field selects which base preset to layer underneath. The `preset` field names the framework preset to apply on top. You do not need to namespace the preset -- just use the short name.

Namespaced syntax like `js/tanstack-start` is not supported in `tako.toml`. Choose your runtime with the `runtime` field and keep `preset` as a plain alias.

### How official presets are resolved

When you deploy or run dev, Tako fetches the official preset definition from the `master` branch of the presets repository. If the fetch fails, preset resolution fails and the deploy is aborted.

For base runtime aliases (`bun`, `node`, `deno`), if their section is missing from the fetched family manifest, Tako falls back to its embedded defaults. Framework presets like `tanstack-start` do not have this fallback -- they must be found in the fetched manifest.

After resolution, Tako writes the resolved preset metadata to `.tako/build.lock.json` with `preset_ref`, `repo`, `path`, and `commit` fields. This file is used for cache-key inputs and visibility into what was resolved.

### Pinning a preset version

By default, unpinned presets fetch from `master` on every resolve. To lock a preset to a specific commit, append `@<commit-hash>`:

```toml
preset = "tanstack-start@a1b2c3d"
```

This guarantees reproducible builds regardless of upstream preset changes.

## Preset TOML format

Preset definitions are TOML sections within a family file. Presets are metadata-only -- they define defaults for entrypoint and assets, nothing else.

### Supported fields

- **`name`** (optional) -- Display name. Defaults to the TOML section name.
- **`main`** (optional) -- Default app entrypoint. If your `tako.toml` sets `main`, it takes precedence.
- **`assets`** (optional) -- List of directories to merge into `public/` after build.

Presets do **not** contain build commands, install commands, start commands, or dev commands. All build configuration belongs in your app's `tako.toml` under `[build]` or `[[build_stages]]`.

### Example preset definition

```toml
[tanstack-start]
main = "@tanstack/react-start/server-entry"
assets = ["dist/client"]
```

This tells Tako that TanStack Start apps use `@tanstack/react-start/server-entry` as their entrypoint and need `dist/client` merged into `public/` for static asset serving. Build commands are configured in your `tako.toml`.

## Preset family files

Official preset definitions are organized by language: `presets/<language>/<language>.toml`. Currently there is one family file:

```
presets/javascript/javascript.toml
```

This file contains all JavaScript/TypeScript framework presets as TOML sections. Each section name is the preset alias:

```toml
# presets/javascript/javascript.toml

[tanstack-start]
main = "@tanstack/react-start/server-entry"
assets = ["dist/client"]
```

During `tako init`, Tako fetches the family manifest to show you available presets for your detected runtime. If no family presets are available after fetch, init skips preset selection and uses the base preset.

## How preset config merges with tako.toml

Presets provide defaults for `main` and `assets`. Your `tako.toml` settings take precedence where applicable.

### Merge rules

**Entrypoint (`main`):** If `main` is set in `tako.toml`, it wins. Otherwise the preset's `main` is used. For JS runtimes, when the preset `main` is an index file like `index.ts` or `src/index.ts`, Tako tries to find the file in your project first, checking `index.<ext>` then `src/index.<ext>`. If neither `tako.toml` nor the preset provides `main`, the deploy fails with guidance.

**Assets:** Preset `assets` are combined with your top-level `assets` in `tako.toml` (deduplicated). Assets are merged into `public/` after the build, with later entries overwriting earlier ones.

**Dev command:** Presets do not define dev commands. Tako always uses the runtime-default dev script:

- Bun: `bun run dev`
- Node: `npm run dev`
- Deno: `deno task dev`

## Build execution

Build commands are configured entirely in your `tako.toml` -- presets do not define build steps. Use `[build]` for a single-stage build or `[[build_stages]]` for multi-stage pipelines:

```toml
# Single-stage build
[build]
run = "vinxi build"
install = "bun install"
```

```toml
# Multi-stage build
[[build_stages]]
name = "frontend"
cwd = "frontend"
install = "bun install"
run = "bun run build"

[[build_stages]]
name = "backend"
run = "bun run build:server"
```

At runtime on the server, the package manager's production install command runs for dependency setup.
