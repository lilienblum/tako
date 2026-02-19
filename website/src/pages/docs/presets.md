---
layout: ../../layouts/DocsLayout.astro
title: Tako Docs - Presets
heading: Presets
current: presets
---

# Presets

Presets define build/runtime defaults for `tako dev` and `tako deploy`.

## Set A Preset In `tako.toml`

```toml
runtime = "bun"
preset = "tanstack-start"
```

## Preset Formats

- Runtime-local aliases:
  - `tanstack-start` (resolved under selected top-level `runtime`)
- Pinned refs:
  - `tanstack-start@<commit-hash>`

Namespaced aliases in `tako.toml` (for example `js/tanstack-start`) are rejected; choose runtime with top-level `runtime` and keep `preset` runtime-local.

## Official Preset Layout

If you are contributing to the official presets repository, use:

```text
presets/js.toml
```

Example:

```text
presets/js.toml
```

## Preset File Schema

Preset names default to the section name, so explicit `name` is usually unnecessary.

```toml
[tanstack-start]
# name = "tanstack-start" # Optional; defaults to section name
main = "dist/server/tako-entry.mjs" # Optional default app entry

[tanstack-start.build]
# assets = ["dist/client"]            # Optional static assets merged into public/
# exclude = ["dist/**/*.map"]        # Optional artifact excludes
# targets = ["linux-x86_64-glibc"]  # Optional target labels
# container = true                  # Optional build mode override
```

Runtime base presets (`bun`, `node`, `deno`) provide default lifecycle commands (`dev`, `install`, `start`, `[build].install`, `[build].build`), default build filters/targets, and default `assets`.
Preset `build.exclude` adds extra patterns on top of runtime-base excludes (base-first, deduplicated), while preset `build.assets` replace runtime-base assets when set.
JS runtime base presets use `mise` when available for local install/build steps, but do not require it; deploy `start` commands run through `mise` so server runtime follows packaged `mise.toml`.

### Supported Keys

- Top-level:
  - `name` (optional)
  - `main` (optional)
  - `dev`, `install`, `start` (optional advanced overrides)
- `[build]`:
  - `assets` (optional)
  - `exclude` (optional)
  - `install`, `build` (optional advanced overrides)
  - `targets` (optional)
  - `container` (optional)

### Not Supported

- Legacy `[artifact]`, `[dev]`, `[deploy]`
- top-level `assets`, `include`, `exclude`, `builder_image`, `runtime`, `id`, `dev_cmd`
- top-level `[targets]`, `[build].builder_image`, and `[build].docker`
