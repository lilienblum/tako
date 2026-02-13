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

## Build and Test

```bash
cd sdk
bun install
bun run build
bun run typecheck
bun test
```

## Related Docs

- `../website/src/content/docs/guides/quickstart.md`
- `examples/js/bun/README.md`
