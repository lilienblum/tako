# tako.sh SDK

Current SDK implementation for JavaScript/TypeScript apps running on Tako.

Package name: `tako.sh`

## What It Provides

- Runtime adapters for Bun, Node.js, and Deno.
- Built-in internal status endpoint:
  - `GET /status` when `Host: tako.internal`
- Optional lifecycle integration hooks (for example config reload handling).

## Install

```bash
bun add tako.sh
```

## Basic Usage

```ts
import { Tako } from "tako.sh";

const app = new Tako({
  fetch: async (req) => new Response("ok"),
});

export default app;
```

Runtime-specific imports are also available:

```ts
import { Tako as BunTako } from "tako.sh/bun";
import { Tako as NodeTako } from "tako.sh/node";
import { Tako as DenoTako } from "tako.sh/deno";
```

## Vite Plugin

Use the Vite plugin to prepare a deploy entry wrapper for Tako.

```ts
import { defineConfig } from "vite";
import { takoVitePlugin } from "tako.sh/vite";

export default defineConfig({
  plugins: [takoVitePlugin()],
});
```

On build, the plugin:

- emits `<outDir>/tako-entry.mjs`, which normalizes your compiled server module into a default-exported fetch handler

On dev (`vite dev`), the plugin:

- adds `.tako.local` to `server.allowedHosts`
- binds Vite to `127.0.0.1:$PORT` with `strictPort: true` when `PORT` is provided

Deploy entry resolution uses `main` from `tako.toml`, then preset top-level `main`.
For Vite apps, point `tako.toml main` at the generated wrapper, for example:

```toml
main = "dist/server/tako-entry.mjs"
```

## Build and Test

```bash
cd sdk
bun install
bun run build
bun run typecheck
bun test
```

## Related Docs

- `../website/src/pages/docs/quickstart.md`
- `examples/js/bun/README.md`
