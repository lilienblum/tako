---
layout: ../../layouts/DocsLayout.astro
title: Tako Docs - Framework Guides
heading: Framework Guides
current: framework-guides
description: "Framework-specific guides for deploying with Tako — fetch handlers, Next.js, Astro, SvelteKit, Nuxt, TanStack Start, and more."
---

# Framework Guides

## Fetch Handler (any runtime)

Tako apps export a standard fetch handler. No adapter import needed:

```ts
export default function fetch(request: Request, env: Record<string, string>) {
  return new Response("Hello from Tako");
}
```

Tako automatically runs your app with the correct runtime (Bun, Node.js, or Deno) based on your project configuration.

## Vite / SSR Frameworks

For SSR frameworks (TanStack Start, Nuxt, SolidStart, etc.), use the Vite plugin:

```ts
import { defineConfig } from "vite";
import { tako } from "tako.sh/vite";

export default defineConfig({
  plugins: [tako()],
});
```

Point `main` in `tako.toml` at the generated wrapper:

```toml
main = "dist/server/tako-entry.mjs"
```
