---
name: tako-sdk
description: >-
  tako.sh SDK: fetch handler interface, Tako class for secrets and build info,
  Vite plugin for SSR frameworks, types reference.
type: framework
library: tako.sh
library_version: "0.0.1"
sources:
  - lilienblum/tako:sdk/javascript/src
---

# Tako SDK (`tako.sh`)

Runtime SDK for JavaScript/TypeScript apps deployed with Tako.

> **CRITICAL**: Tako uses the **web-standard fetch handler** interface. Your app exports a function `(Request, env) => Response`. No proprietary API required — the SDK is optional.

> **CRITICAL**: The Vite plugin (`tako.sh/vite`) is only for SSR/server framework builds (e.g. TanStack Start, Nuxt, SolidStart). Plain fetch-handler apps do not need it.

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

| Import path    | Purpose                    | Key exports              |
| -------------- | -------------------------- | ------------------------ |
| `tako.sh`      | Core utilities             | `Tako` class, types      |
| `tako.sh/vite` | Vite plugin for SSR builds | `tako()` plugin function |

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

- Reads from a mutable store populated at runtime via tako-server
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

### 2. HIGH: Assuming secrets are available at import time

```typescript
// WRONG — secrets may not be populated yet at module load
const DB_URL = Tako.secrets.DATABASE_URL; // may be undefined

// CORRECT — access secrets at request time
export default function fetch(req: Request) {
  const dbUrl = Tako.secrets.DATABASE_URL; // always current
  // ...
}
```

### 3. HIGH: Serializing the secrets object

```typescript
// WRONG — bulk access is redacted
console.log(JSON.stringify(Tako.secrets)); // "[REDACTED]"

// CORRECT — access individual secrets by name
const dbUrl = Tako.secrets.DATABASE_URL;
```
