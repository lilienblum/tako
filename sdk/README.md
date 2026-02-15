# tako.sh SDK

Current SDK implementation for JavaScript/TypeScript apps running on Tako.

Package name: `tako.sh`

## What It Provides

- Runtime adapters for Bun, Node.js, and Deno.
- Built-in Tako endpoints:
  - `/_tako/status`
  - `/_tako/health`
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

Use the Vite plugin to normalize build output into Tako's deploy artifact contract.

```ts
import { defineConfig } from "vite";
import { takoVitePlugin } from "tako.sh/vite";

export default defineConfig({
  plugins: [
    takoVitePlugin({
      // Optional when your client output is not named "client"
      clientDir: "dist/web",
      // Optional (auto-detected as dist/server when present)
      serverDir: "dist/ssr",
    }),
  ],
});
```

Staging output defaults to `.tako/artifacts/app`:

- `.tako/artifacts/app/static` = `public/` merged with client build output (client output wins on conflicts)
- `.tako/artifacts/app/server` = copied server output (if configured/detected)

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
