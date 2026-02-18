---
layout: ../../layouts/DocsLayout.astro
title: Tako Docs - Preset Reference
heading: Preset Reference
current: presets
---

# Preset Reference

Presets define build/runtime defaults for `tako dev` and `tako deploy`.

## Set A Preset In `tako.toml`

```toml
runtime = "bun"
preset = "bun/tanstack-start"
```

You can also reference your own preset file directly from GitHub:

```toml
preset = "github:username/my-presets/custom-preset.toml"
```

## Preset Reference Formats

- Official aliases:
  - `bun`
  - `node`
  - `deno`
  - `bun/tanstack-start`
- Pinned refs:
  - `bun@<commit-hash>`
  - `bun/tanstack-start@<commit-hash>`
  - legacy alias pin format `bun/<commit-hash>` is still accepted
- GitHub refs:
  - `github:<owner>/<repo>/<path>.toml`
  - `github:<owner>/<repo>/<path>.toml@<commit-hash>`

## Official Preset Layout

If you are contributing to the official presets repository, use:

```text
presets/<runtime>/<name>.toml
```

Example:

```text
presets/bun/tanstack-start.toml
```

## Preset File Schema

Preset names default to the file name, so top-level `name` is usually unnecessary.

```toml
# name = "tanstack-start" # Optional; defaults to file name
main = "dist/server/tako-entry.mjs" # Optional default app entry
assets = ["dist/client"]            # Optional static assets merged into public/

[build]
# exclude = ["dist/**/*.map"]        # Optional artifact excludes
# targets = ["linux-x86_64-glibc"]  # Optional target labels
# container = true                  # Optional build mode override
```

Runtime base presets (`bun`, `node`, `deno`) provide default lifecycle commands (`dev`, `install`, `start`, `[build].install`, `[build].build`), default build filters/targets, and default `assets`.
Preset `build.exclude` adds extra patterns on top of runtime-base excludes (base-first, deduplicated), while preset `assets` replace runtime-base assets when set.

### Supported Keys

- Top-level:
  - `name` (optional)
  - `main` (optional)
  - `assets` (optional)
  - `dev`, `install`, `start` (optional advanced overrides)
- `[build]`:
  - `exclude` (optional)
  - `install`, `build` (optional advanced overrides)
  - `targets` (optional)
  - `container` (optional; deprecated alias `docker`)

### Not Supported

- Legacy `[artifact]`, `[dev]`, `[deploy]`
- top-level `include`, `exclude`, `builder_image`, `runtime`, `id`
- top-level `[targets]` and `[build].builder_image`
