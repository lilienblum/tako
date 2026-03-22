---
name: lifecycle/deploy-to-tako
description: >-
  Step-by-step guide to deploy a JavaScript/TypeScript app with Tako:
  tako.toml configuration, fetch handler setup, Vite plugin for SSR
  frameworks, secrets, build stages.
type: lifecycle
library: tako.sh
library_version: "0.0.1"
requires:
  - tako-sdk
sources:
  - lilienblum/tako:SPEC.md
---

# Deploy a JavaScript/TypeScript App with Tako

This guide covers adding Tako to a new or existing JS/TS project. Complete the steps in order.

> **CRITICAL**: Tako uses the web-standard fetch handler interface. Your app exports `(Request, env) => Response`. No proprietary runtime API is required — the SDK is optional.

> **CRITICAL**: `tako.toml` is your project's deployment config. It lives in the project root alongside `package.json`.

## Step 1: Install the SDK

```bash
npm install tako.sh
# or: bun add tako.sh / pnpm add tako.sh
```

## Step 2: Create tako.toml

```toml
# tako.toml — minimal config for a plain fetch handler app
main = "src/index.ts"
```

With a build step:

```toml
main = "dist/index.mjs"

[build]
run = "npm run build"
```

With a preset (for frameworks like TanStack Start):

```toml
preset = "tanstack-start"
```

### Key tako.toml fields

| Field             | Purpose                                               |
| ----------------- | ----------------------------------------------------- |
| `main`            | Server entrypoint (file exporting a fetch handler)    |
| `preset`          | App preset name (provides `main` + `assets` defaults) |
| `assets`          | Static assets directory to serve                      |
| `package_manager` | `bun`, `npm`, `pnpm`, `yarn` (auto-detected)          |
| `[build]`         | Build configuration                                   |
| `[build].run`     | Build command (e.g. `npm run build`)                  |
| `[build].install` | Install command (overrides auto-detected)             |
| `[build].cwd`     | Working directory for build (for monorepos)           |

## Step 3: Write Your App

### Plain fetch handler (no framework)

```typescript
// src/index.ts
export default function fetch(request: Request, env: Record<string, string>) {
  const url = new URL(request.url);

  if (url.pathname === "/api/health") {
    return Response.json({ status: "ok" });
  }

  return new Response("Hello from Tako!");
}
```

No SDK import needed. This is a complete, deployable Tako app.

### With SSR framework (Vite plugin)

```typescript
// vite.config.ts
import { defineConfig } from "vite";
import { tako } from "tako.sh/vite";

export default defineConfig({
  plugins: [tako()],
});
```

```toml
# tako.toml
preset = "tanstack-start"
# or for custom SSR:
# main = "dist/server/tako-entry.mjs"
```

The Vite plugin emits `tako-entry.mjs` in your build output that normalizes any SSR server entry into Tako's fetch handler format.

## Step 4: Configure Secrets

Secrets are managed via the Tako CLI:

```bash
tako secret set DATABASE_URL "postgres://..."
tako secret set API_KEY "sk-..."
```

Access in code:

```typescript
import { Tako } from "tako.sh";

export default function fetch(request: Request) {
  const dbUrl = Tako.secrets.DATABASE_URL;
  // ...
}
```

Secrets are injected at runtime by tako-server, not baked into the build.

## Step 5: Deploy

```bash
tako deploy
```

This builds locally (if `[build]` is configured), uploads the artifact to your Tako server, and performs a rolling update.

## Build Stages (Monorepos)

For monorepo projects with multiple build steps:

```toml
main = "apps/web/dist/server/tako-entry.mjs"

[[build_stages]]
name = "packages"
run = "npm run build"
cwd = "packages/shared"

[[build_stages]]
name = "app"
run = "npm run build"
cwd = "apps/web"
```

`cwd` allows `..` for monorepo traversal (guarded against root escape).
`[[build_stages]]` is mutually exclusive with `[build].run`.

## Local Development

```bash
tako dev
```

This runs your app locally with:

- Local HTTPS via auto-generated certificates
- `.tako.test` domain for local development
- Hot reload support (passes through to your dev server)

## Common Mistakes

### 1. CRITICAL: Missing fetch handler export

```typescript
// WRONG — no default export
export function handler(req: Request) {
  return new Response("Hello");
}

// CORRECT — must be a default export
export default function fetch(req: Request) {
  return new Response("Hello");
}
```

### 2. CRITICAL: Wrong main path in tako.toml

```toml
# WRONG — pointing to source when there's a build step
main = "src/index.ts"
[build]
run = "npm run build"

# CORRECT — point to the build output
main = "dist/index.mjs"
[build]
run = "npm run build"
```

### 3. HIGH: Hardcoding secrets in source

```typescript
// WRONG — secrets in code
const DB_URL = "postgres://user:pass@host/db";

// CORRECT — use Tako secrets at request time
import { Tako } from "tako.sh";

export default function fetch(req: Request) {
  const dbUrl = Tako.secrets.DATABASE_URL;
  // ...
}
```

### 4. MEDIUM: Using the Vite plugin without an SSR framework

The `tako()` Vite plugin is specifically for SSR builds that emit a server entry chunk. For client-only Vite apps or plain fetch handler apps, don't use the plugin — just write a fetch handler and set `main` in `tako.toml`.

## Cross-References

- [tako-sdk](../../tako-sdk/SKILL.md) — SDK API reference, Tako class, types
