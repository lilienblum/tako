---
name: tako-sdk
description: >-
  tako.sh SDK: fetch handler interface, Tako class for secrets/build info/channels/workflows,
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

**On `vite dev`:** Adds `.test` to allowed hosts. If `PORT` env var is set, binds Vite to `127.0.0.1:$PORT` with `strictPort: true` (used by `tako dev`).

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

## Channels

Durable pub-sub streams with SSE and WebSocket transport.

### Defining channels (file-based)

Drop one file per channel pattern in `channels/<name>.ts` that default-exports `defineChannel(pattern, config?)`. Imperative registration (`Tako.channels.define()`) no longer exists. Filenames become the accessor key on `Tako.channels.<name>`.

```typescript
// channels/chat.ts
import { defineChannel } from "tako.sh";

type ChatMessages = {
  msg: { text: string; userId: string };
  typing: { userId: string };
};

export default defineChannel<ChatMessages>("chat/:roomId", {
  async auth(request, ctx) {
    // ctx.params.roomId is typed; ctx.operation = "subscribe" | "publish" | "connect"
    const userId = await getUserId(request);
    if (!userId) return false;
    return { subject: userId };
  },
  handler: {
    msg: async (data, ctx) => {
      await db.saveMessage(ctx.params.roomId, data);
      return data; // fanned out to subscribers
    },
    typing: async (data) => data,
  },
  replayWindowMs: 24 * 60 * 60 * 1000,
  inactivityTtlMs: 0,
  keepaliveIntervalMs: 25_000,
  maxConnectionLifetimeMs: 2 * 60 * 60 * 1000,
});
```

- Patterns are Hono-style: `/`-separated segments with `:name` captures and an optional trailing `*` wildcard. Must be a string literal.
- `auth` is optional. Omit for public channels (defaults to allow-all).
- `handler` presence decides transport: present → WebSocket, absent → SSE (broadcast-only). SSE channels reject client POST publishes.

Auth return values: `false` deny · `true` allow anonymously · `{ subject }` allow with identity.

### Publishing messages (server-side)

Use the typed accessor — it's populated at boot from `channels/` discovery. Call signature is `(type, data)`:

```typescript
// Unparameterized channel: direct surface
Tako.channels.status.send("ping", { at: Date.now() });

// Parameterized channel: bind params, then send
Tako.channels.chat({ roomId: "room1" }).send("msg", {
  text: "hello",
  userId,
});
```

### Subscribing / connecting (client-side)

```typescript
import { tako } from "tako.sh/client";

// SSE channel
const sub = tako.channels.status.subscribe({
  ping: (data) => console.log("pong at", data.at),
  alert: (data) => console.warn(data.text),
});
sub.close();

// WS channel — same shape for subscribe, plus .send
const room = tako.channels.chat({ roomId: "room1" });
room.subscribe({
  msg: (data) => console.log(`${data.userId}: ${data.text}`),
  typing: () => {},
});
await room.send("typing", { userId: "me" });
```

### React

`tako.sh/react` exposes a single `useChannel` hook. SSE is the default; pass `transport: "ws"` for WebSocket.

```tsx
import { useChannel } from "tako.sh/react";

function ChatRoom({ room }: { room: string }) {
  const { messages, status, error } = useChannel<{ body: string }>(`chat:${room}`);
  if (error) return <p>error: {error.message}</p>;
  return (
    <ul>
      {messages.map((m) => (
        <li key={m.id}>{m.data.body}</li>
      ))}
    </ul>
  );
}
```

WebSocket with `send`:

```tsx
const { messages, send } = useChannel(`chat:${room}`, { transport: "ws" });
```

Return shape (`ChannelConnection<T>`): `messages` (capped at 500, oldest-first), `status` (`"connecting" | "open"`), `error`, `clear()`, and `send(data)` on WebSocket only.

#### Reacting to messages imperatively

Pass an `onMessage` handler when you want to fire a side effect on each incoming message (toast, external store, ref update) without wiring a `useEffect` around the messages array. The hook uses a latest-ref internally, so the handler does not need to be memoized and swapping it does not reconnect:

```tsx
useChannel("notifications", {
  onMessage: (msg) => toast(msg.data.text),
});
```

#### Sharing one connection across components

Each `useChannel` call opens its own SSE/WebSocket. When multiple components in the same tree need the same channel, call the hook once in a provider and fan out via context:

```tsx
const BroadcastCtx = createContext<ChannelConnection<Msg> | null>(null);

export function BroadcastProvider({ children }: { children: React.ReactNode }) {
  const ch = useChannel<Msg>("demo-broadcast");
  return <BroadcastCtx.Provider value={ch}>{children}</BroadcastCtx.Provider>;
}

export function useBroadcast() {
  const ctx = useContext(BroadcastCtx);
  if (!ctx) throw new Error("useBroadcast outside BroadcastProvider");
  return ctx;
}
```

One connection, one buffer, any number of consumers.

### Network routes

| Direction     | Method | Path                        | Transport                        |
| ------------- | ------ | --------------------------- | -------------------------------- |
| Subscribe     | GET    | `/channels/<name>`          | SSE (`text/event-stream`)        |
| Connect       | GET    | `/channels/<name>`          | WebSocket (`Upgrade: websocket`) |
| Publish       | POST   | `/channels/<name>/messages` | JSON body                        |
| Auth callback | POST   | `/channels/authorize`       | Internal (`Host: tako.internal`) |

## Workflows

Durable background tasks with retries, schedules, and step checkpointing.

### Authoring workflows

Drop a file in `workflows/<name>.ts` with a default export:

```typescript
// workflows/send-email.ts
import { defineWorkflow } from "tako.sh";

export default defineWorkflow(
  async (payload: { userId: string; to: string }, ctx) => {
    const user = await ctx.run("fetch-user", () => db.users.find(payload.userId));
    await ctx.run("send", () => sendEmail(user, payload.to));
  },
  {
    retries: 3, // retries after first attempt (default 2)
    schedule: "0 9 * * *", // cron: daily at 9am (5-field)
    concurrency: 10, // max parallel runs per worker (default 10)
    timeoutMs: 30_000, // handler timeout (default Infinity)
    backoff: { base: 1_000, max: 3_600_000 }, // exponential backoff
  },
);
```

### Enqueuing

```typescript
import { Tako } from "tako.sh";

await Tako.workflows.enqueue("send-email", { userId: "u1", to: "a@b.c" });

await Tako.workflows.enqueue("send-email", payload, {
  runAt: new Date(Date.now() + 60_000), // delay
  retries: 9, // override workflow default
  uniqueKey: "digest:2026-04-14", // idempotency: no-op if non-terminal run exists
});
```

After running `tako typegen`, `enqueue` is type-checked against each workflow's payload type.

### Step API (`ctx`)

| Method                        | Description                                                            |
| ----------------------------- | ---------------------------------------------------------------------- |
| `ctx.run(name, fn, opts?)`    | Memoized step — replays stored result on retry instead of re-executing |
| `ctx.sleep(name, durationMs)` | Durable sleep — short sleeps inline, long sleeps (≥30s) defer the run  |
| `ctx.waitFor<T>(name, opts?)` | Park until `signal(name)` arrives or timeout; returns `T \| null`      |
| `ctx.bail(reason?)`           | End cleanly as `cancelled` (no retries)                                |
| `ctx.fail(error)`             | End as `dead` immediately (no retries)                                 |

`ctx.run` options:

- `retries?: number` — in-step retry attempts (default 0)
- `backoff?: { base?, max? }` — in-step backoff
- `retry: false` — any throw inside `fn` immediately fails the run

`ctx.waitFor` options:

- `timeout?: number` — ms until the step resolves to `null` (default: park indefinitely)

### Signals

```typescript
// Wake all waitFor("approval:order-abc") calls with a payload
await Tako.workflows.signal("approval:order-abc", { approved: true });
```

### Run lifecycle

`pending → running → succeeded | cancelled | dead`

- Throwing a regular error triggers the run-level retry path (exponential backoff).
- `ctx.bail()` → `cancelled`, no retries.
- `ctx.fail()` → `dead`, no retries.

### tako.toml configuration

```toml
[servers.workflows]        # default for all servers in the env
workers = 1                # 0 = scale-to-zero (default)
concurrency = 10

[servers.lax.workflows]    # per-server override
workers = 2
```

- `workers = 0` — scale-to-zero: worker spawned on first enqueue/cron tick, exits after 300s idle.
- Precedence: `[servers.<name>.workflows]` > `[servers.workflows]` > defaults.
- If a `workflows/` directory exists but no `[servers.*.workflows]` block, the app is implicitly scale-to-zero on every server.

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
