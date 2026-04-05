# tako.sh SDK

JavaScript/TypeScript SDK for apps running on Tako.

Package name: `tako.sh`

## What It Provides

- `Tako` class with secrets management and build info
- Vite plugin for SSR framework builds
- Built-in internal status endpoint (`GET /status` on `Host: tako`)

## Install

```bash
bun add tako.sh
```

## Basic Usage

```ts
export default function fetch(req: Request, env: Record<string, string>) {
  return new Response("ok");
}
```

## Vite Plugin

Use the Vite plugin to prepare a deploy entry wrapper for Tako.

```ts
import { defineConfig } from "vite";
import { tako } from "tako.sh/vite";

export default defineConfig({
  plugins: [tako()],
});
```

On build, the plugin:

- emits `<outDir>/tako-entry.mjs`, which normalizes your compiled server module into a default-exported fetch handler

On dev (`vite dev`), the plugin:

- adds `.tako` to `server.allowedHosts`
- binds Vite to `127.0.0.1:$PORT` with `strictPort: true` when `PORT` is provided

Deploy entry resolution uses `main` from `tako.toml`, then preset top-level `main`.
For Vite apps, point `tako.toml main` at the generated wrapper, for example:

```toml
main = "dist/server/tako-entry.mjs"
```

If your app uses Vite or another JS workspace tool behind package scripts, keep using this plugin. Tako's JS defaults run the runtime lane's `dev` / `build` scripts, so those scripts are the right place to call `vp`, `turbo`, or similar tools.

## Next.js Adapter

Use the Next.js helper to enable standalone output plus the Tako adapter:

```ts
import { withTako } from "tako.sh/nextjs";

export default withTako({
  // your existing Next config
});
```

On build, the adapter:

- forces `output: "standalone"`
- writes `.next/tako-entry.mjs`
- copies `public/` and `.next/static/` into `.next/standalone/` when Next emits standalone output

The generated wrapper prefers `.next/standalone/server.js` when it exists. If Next does not emit standalone output for the current build pipeline, the wrapper falls back to spawning `next start` against the built `.next/` directory.

For Tako projects using the `nextjs` preset, the generated deploy entrypoint is:

```toml
preset = "nextjs"
# main defaults to .next/tako-entry.mjs
```

## Build and Test

```bash
cd sdk/javascript
bun install
bun run build
bun run typecheck
bun test
```

## Related Docs

- `../../website/src/pages/docs/quickstart.md`
- `../../examples/javascript/demo/README.md`
