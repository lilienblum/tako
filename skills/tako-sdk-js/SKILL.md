---
name: tako-sdk
description: >-
  tako.sh SDK: fetch handler interface, Tako class for secrets and build info,
  Vite and Next.js adapters for framework builds, types reference.
type: framework
library: tako.sh
library_version: "0.0.1"
sources:
  - lilienblum/tako:sdk/javascript/src
---

# Tako SDK (`tako.sh`)

Runtime SDK for JavaScript/TypeScript apps deployed with Tako.

> **CRITICAL**: The `tako.sh` package is **required** — it provides the entrypoint binaries (`tako-bun`, `tako-node`, `tako-deno`) that tako-server launches to run your app. Your app exports a standard fetch handler `(Request, env) => Response`, but importing the `Tako` class is optional.

> **CRITICAL**: Framework helpers are opt-in. Use `tako.sh/vite` for Vite-based SSR frameworks (TanStack Start, Nuxt, SolidStart) and `tako.sh/nextjs` for Next.js standalone builds. Plain fetch-handler apps do not need either helper.

## Core Concept: The Fetch Handler

Tako apps export a standard fetch handler as the default export:

```typescript
// src/index.ts — this is a complete Tako app, no SDK import needed
export default function fetch(request: Request, env: Record<string, string>) {
  return new Response("Hello World!");
}
```

The handler signature is:

```typescript
type FetchHandler = (request: Request, env: Record<string, string>) => Response | Promise<Response>;
```

Two export forms are supported:

```typescript
// Form 1: default export is the fetch function
export default function fetch(req: Request, env: Record<string, string>) {
  return new Response("OK");
}

// Form 2: default export is an object with a fetch method
export default {
  fetch(req: Request, env: Record<string, string>) {
    return new Response("OK");
  },
};
```

## Package Exports

| Import path      | Purpose                              | Key exports                                                         |
| ---------------- | ------------------------------------ | ------------------------------------------------------------------- |
| `tako.sh`        | Core utilities                       | `Tako` class, types                                                 |
| `tako.sh/vite`   | Vite plugin for SSR builds           | `tako()` plugin function                                            |
| `tako.sh/nextjs` | Next.js standalone adapter + wrapper | `withTako()`, `createNextjsAdapter()`, `createNextjsFetchHandler()` |

## Tako Class

Optional utilities for Tako apps:

```typescript
import { Tako } from "tako.sh";

// Check if running in Tako
if (Tako.isRunningInTako()) {
  console.log(`Build: ${Tako.build}`);
}

// Access secrets at request time
export default function fetch(request: Request) {
  const dbUrl = Tako.secrets.DATABASE_URL;
  return new Response(`Connected to ${dbUrl ? "db" : "nothing"}`);
}
```

### Secrets

`Tako.secrets` is a Proxy that:

- Reads from a mutable store populated via fd 3 at startup (before user module is imported)
- Individual access works: `Tako.secrets.MY_KEY` returns the string value
- Resists bulk serialization: `toString()`, `toJSON()` return `"[REDACTED]"`
- Keys are enumerable: `Object.keys(Tako.secrets)` works

### Static Properties and Methods

- `Tako.secrets` — Proxy object for environment secrets
- `Tako.build` — Returns build version (from `TAKO_BUILD` env var)
- `Tako.isRunningInTako()` — Returns `true` when running in Tako environment

## Vite Plugin

For SSR framework builds (TanStack Start, Nuxt, SolidStart, etc.):

```typescript
// vite.config.ts
import { defineConfig } from "vite";
import { tako } from "tako.sh/vite";

export default defineConfig({
  plugins: [tako()],
});
```

**On `vite build`:** Emits `<outDir>/tako-entry.mjs` — a wrapper that normalizes the compiled server module into a default-exported fetch handler. Point `main` in `tako.toml` at this file.

**On `vite dev`:** Adds `.tako.test` to allowed hosts. If `PORT` env var is set, binds Vite to `127.0.0.1:$PORT` with `strictPort: true` (used by `tako dev`).

## Next.js Adapter

For Next.js standalone builds:

```typescript
// next.config.mjs
import { withTako } from "tako.sh/nextjs";

export default withTako({});
```

`withTako()` sets `output = "standalone"` and points `adapterPath` at the Tako adapter shipped in the SDK.

On `next build`, the adapter:

- copies `public/` into `.next/standalone/public/` when standalone output exists
- copies `.next/static/` into `.next/standalone/.next/static/` when standalone output exists
- writes `.next/tako-entry.mjs`

The generated wrapper prefers `.next/standalone/server.js` when it exists. Otherwise it falls back to `next start`.

Point your Tako deploy `main` at `.next/tako-entry.mjs`, or use the `nextjs` preset so that default is provided for you.

## Types

```typescript
import type { FetchHandler, TakoOptions, TakoStatus } from "tako.sh";

// FetchHandler = (request: Request, env: Record<string, string>) => Response | Promise<Response>

// TakoStatus — returned by the internal health endpoint
interface TakoStatus {
  status: "healthy" | "starting" | "draining" | "unhealthy";
  app: string;
  version: string;
  instance_id: string;
  pid: number;
  uptime_seconds: number;
}
```

## Common Mistakes

### 1. CRITICAL: Using the Vite plugin for non-SSR apps

```typescript
// WRONG — plain fetch handler app doesn't need the Vite plugin
// vite.config.ts with tako() plugin + src/index.ts with a fetch handler

// CORRECT — the Vite plugin is only for SSR framework builds
// For plain apps, just export a fetch handler and set main in tako.toml
```

### 2. HIGH: Forgetting the Next.js helper for standalone deploys

```typescript
// WRONG — plain Next config without the Tako helper
export default {};

// CORRECT — let Tako configure standalone output and adapterPath
import { withTako } from "tako.sh/nextjs";

export default withTako({});
```

### 3. HIGH: Serializing the secrets object

```typescript
// WRONG — bulk access is redacted
console.log(JSON.stringify(Tako.secrets)); // "[REDACTED]"

// CORRECT — access individual secrets by name
const dbUrl = Tako.secrets.DATABASE_URL;
```
