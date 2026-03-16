---
layout: ../../layouts/DocsLayout.astro
title: "Tako Docs - Presets"
heading: Presets
current: presets
---

# Presets

Presets provide build and runtime defaults so you can deploy without wiring up boilerplate config. When you run `tako deploy` or `tako dev`, Tako resolves a preset that tells it how to install dependencies, build your app, and start it on the server.

Every app uses a preset. If you do not set one explicitly, Tako picks the **base preset** for your detected runtime.

## Base presets

Tako ships with three built-in runtime base presets: `bun`, `node`, and `deno`. These are compiled into the CLI -- they are never loaded from files on disk.

Each base preset defines sensible defaults for its runtime:

| Field | Bun | Node | Deno |
|---|---|---|---|
| `main` | `src/index.ts` | `index.js` | `main.ts` |
| `dev` | `bun --hot {main}` | `node {main}` | `deno run --watch --allow-net --allow-env --allow-read {main}` |
| `build.exclude` | `node_modules/` | `node_modules/` | -- |
| `build.container` | `false` | `false` | `false` |

All three JS base presets target four Linux architectures by default:

```
linux-x86_64-glibc
linux-aarch64-glibc
linux-x86_64-musl
linux-aarch64-musl
```

Because JS base presets set `build.container = false`, builds run on your local machine unless a framework preset explicitly enables container builds.

Base presets also define lifecycle commands for dependency installation, build steps, and production start commands.

## Official presets

Official presets handle framework-specific concerns on top of a base preset. For example, `tanstack-start` knows the right entrypoint and asset directory for TanStack Start apps.

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

Preset definitions are TOML sections within a family file. Here are the supported fields:

### Top-level fields

- **`name`** (optional) -- Display name. Defaults to the TOML section name.
- **`main`** (optional) -- Default app entrypoint. If your `tako.toml` sets `main`, it takes precedence.
- **`dev`** (optional) -- Dev command override. Only used when `preset` is explicitly set in `tako.toml`.
- **`install`** (optional) -- Server-side dependency install command.
- **`start`** (optional) -- Production start command for the server.

### `[build]` section

- **`assets`** (optional) -- List of directories to merge into `public/` after build. Replaces base preset assets when set.
- **`exclude`** (optional) -- Glob patterns for files to exclude from the deploy artifact. Appended to base preset excludes (deduplicated).
- **`install`** (optional) -- Build-time dependency install command.
- **`build`** (optional) -- Build-time compilation/bundling command.
- **`targets`** (optional) -- Target platform labels, e.g. `["linux-x86_64-glibc"]`. Overrides base preset targets when set.
- **`container`** (optional) -- `true` to build in Docker, `false` to build on the local host. Overrides base preset default when set.

Presets do **not** support `[[build.stages]]`. Custom build stages belong in your app's `tako.toml`.

### Example preset definition

```toml
[tanstack-start]
main = "dist/server/tako-entry.mjs"

[tanstack-start.build]
assets = ["dist/client"]
```

This tells Tako that TanStack Start apps use `dist/server/tako-entry.mjs` as their entrypoint and need `dist/client` merged into `public/` for static asset serving.

### Rejected keys

The following keys and sections are not allowed in preset files:

- Sections: `[artifact]`, `[dev]`, `[deploy]`
- Top-level keys: `assets`, `include`, `exclude`, `builder_image`, `runtime`, `id`, `dev_cmd`
- Build keys: `[build].builder_image`, `[build].docker`

## Preset family files

Official preset definitions are organized by runtime family in `presets/<family>.toml`. Currently there is one family file:

```
presets/js.toml
```

This file contains all JavaScript/TypeScript framework presets as TOML sections. Each section name is the preset alias:

```toml
# presets/js.toml

[tanstack-start]
main = "dist/server/tako-entry.mjs"

[tanstack-start.build]
assets = ["dist/client"]
```

During `tako init`, Tako fetches the family manifest to show you available presets for your detected runtime. If no family presets are available after fetch, init skips preset selection and uses the base preset.

## How preset config merges with tako.toml

Presets provide defaults. Your `tako.toml` settings take precedence where applicable.

### Merge rules

**Entrypoint (`main`):** If `main` is set in `tako.toml`, it wins. Otherwise the preset's `main` is used. For JS runtimes, when the preset `main` is an index file like `index.ts` or `src/index.ts`, Tako tries to find the file in your project first, checking `index.<ext>` then `src/index.<ext>`. If neither `tako.toml` nor the preset provides `main`, the deploy fails with guidance.

**Build excludes:** Preset `build.exclude` patterns are appended to the base preset's excludes (deduplicated). Your app's `build.exclude` in `tako.toml` is then appended on top. The base preset's patterns always come first.

**Build assets:** Preset `build.assets` replaces the base preset's assets when set. Your app's `build.assets` in `tako.toml` is added after the preset's (deduplicated). Assets are merged into `public/` after the build, with later entries overwriting earlier ones.

**Build targets and container mode:** Preset `build.targets` and `build.container` override base preset defaults when explicitly set. Your app's `tako.toml` settings override the preset in turn.

**Dev command:** When `preset` is explicitly set in `tako.toml`, `tako dev` uses the preset's `dev` command. When `preset` is omitted (using just the base preset), `tako dev` ignores the preset's `dev` and runs a runtime-default command:

- Bun: `bun run node_modules/tako.sh/src/entrypoints/bun.ts {main}`
- Node: `node --experimental-strip-types node_modules/tako.sh/src/entrypoints/node.ts {main}`
- Deno: `deno run --allow-net --allow-env --allow-read node_modules/tako.sh/src/entrypoints/deno.ts {main}`

## Build stage execution order

During deploy, build commands run in a fixed order for each target:

1. **Preset stage** -- `build.install` runs first, then `build.build` (when the preset defines them)
2. **App stages** -- `[[build.stages]]` from `tako.toml` run in declaration order (each stage runs its `install` then `run`)

This means the preset handles foundational setup (dependency installation, framework compilation) and your custom stages run afterward for any additional processing.

At runtime on the server, the preset's top-level `install` command runs for dependency setup (e.g., production `bun install`), and the preset's `start` command launches the app.