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

Use the Vite plugin to emit deploy metadata for Tako.

```ts
import { defineConfig } from "vite";
import { takoVitePlugin } from "tako.sh/vite";

export default defineConfig({
  plugins: [takoVitePlugin()],
});
```

On build, the plugin writes metadata to deploy root:

- forces `ssr.noExternal = true` so SSR bundles run from deploy dist without `node_modules`
- emits `<outDir>/tako-entry.mjs`, a wrapped server entry that handles internal
  `Host: tako.internal` + `/status` and forwards other requests to the compiled app entry
- default: `<build.outDir>/.tako-vite.json`
- when `build.outDir` ends with `server`: parent directory (for example `dist/.tako-vite.json` with `compiled_main` prefixed by `server/`)

- `compiled_main`: wrapped runtime entry path (`tako-entry.mjs`)
- `entries`: all build entry chunk filenames

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
