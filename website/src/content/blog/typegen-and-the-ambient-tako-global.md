---
title: "tako typegen and the Ambient Tako Global"
date: "2026-04-18T01:01"
description: "Tako installs a frozen Tako global before your code runs and tako typegen types every secret, channel, workflow, and env var — zero imports, zero silent typos."
image: 402b4ee3a4e4
---

Most runtime config is reached through APIs that lie to you. `process.env` pretends every variable is a string and returns `undefined` when you typo a name. `process.env.DATBASE_URL` is a syntactically valid read that fails silently, then explodes somewhere downstream — usually at 2am, usually in production.

Tako's JavaScript SDK now ships a different shape. A frozen `Tako` object is installed on `globalThis` before your entrypoint's module graph starts loading. `tako typegen` writes a `tako.d.ts` that tells TypeScript exactly which secrets, channels, workflows, and env vars you actually have. No imports, no guessing, no silent typos.

## What the ambient Tako gives you

Every Tako JS/TS app runs inside an entrypoint shim that calls `installTakoGlobal()` before your `main` module is imported. By the time your first line of app code runs, `Tako` is already there — defined with `writable: false`, `configurable: false`, and frozen, so nothing deeper in the dependency tree can reassign or shadow it.

```ts
// Anywhere in your app — no import
Tako.secrets.DATABASE_URL; // typed string
Tako.channels.chat({ roomId }); // typed accessor
Tako.workflows.enqueue("send-email", { to });
Tako.env; // "development" | "production"
Tako.isDev; // boolean
Tako.port; // number, assigned by Tako
Tako.dataDir; // persistent path, survives deploys
Tako.build; // deploy-time build ID
Tako.logger.info("hello", { userId });
```

It's the same surface whether you're on Bun, Node, or Deno — each runtime entrypoint calls `installTakoGlobal()` during boot and you get the same object. The standalone `Tako` import from `tako.sh` still works for code that prefers explicit imports; the global is just the zero-ceremony version of the same thing.

## What `tako typegen` generates

[`tako typegen`](/docs/cli) is the other half. It scans your project and emits a `tako.d.ts` that augments the empty placeholder `TakoSecrets` with your actual secret names, extends `NodeJS.ProcessEnv` and `ImportMetaEnv` with the standard Tako runtime vars, and emits module-augmentation blocks for channels and workflows.

| Source                                   | What typegen emits                                                                        |
| ---------------------------------------- | ----------------------------------------------------------------------------------------- |
| `.tako/secrets.json` (encrypted)         | `interface TakoSecrets { readonly DATABASE_URL: string; ... }`                            |
| `channels/chat.ts`, `channels/status.ts` | `interface Channels { "chat": typeof ...; "status": typeof ...; }`                        |
| `workflows/send-email.ts`                | `interface Workflows { "send-email": InferWorkflowPayload<...> }`                         |
| Runtime env                              | `ENV`, `PORT`, `HOST`, `TAKO_BUILD`, `TAKO_DATA_DIR` on `process.env` / `import.meta.env` |

Secret names are plaintext in [`.tako/secrets.json`](/blog/secrets-without-env-files) — the values aren't — so typegen can emit the type surface without ever touching your encryption key. When you add a secret with `tako secrets set`, or a new file to `channels/` or `workflows/`, typegen picks it up and rewrites `tako.d.ts` on the next `tako dev` or `tako deploy`.

The file lands somewhere TypeScript's default `include` will find: next to an existing copy if you have one, or inside `src/` or `app/` if those directories exist, or at the project root. No `tsconfig.json` edits needed.

## Why this is safer than `process.env`

`process.env` is fundamentally a `string → string` map. `process.env.DATBASE_URL` is a valid read; it just returns `undefined`. Your editor can't warn you because the shape of `process.env` isn't tied to your actual secrets.

`Tako.secrets.DATBASE_URL` is a compile error. `Tako.workflows.enqueue("sennd-email", ...)` is a compile error. `Tako.channels.chta({ roomId })` is a compile error. If TypeScript sees your file, it'll catch these before they ever run.

A few more guarantees that `process.env` can't match:

- **Frozen surface.** Code lower in the dependency tree can't swap `Tako.secrets` for a logger or a honeypot — the object is sealed at install time. That matters, because [secrets never hit disk on the server](/blog/secrets-without-env-files) and the runtime surface is the only place they live.
- **Redaction by default.** `String(Tako.secrets)` returns `"[REDACTED]"`. `JSON.stringify(Tako.secrets)` returns `"[REDACTED]"`. Log the whole object by accident and no values leak.
- **Server-only.** The global is installed on the Node/Bun/Deno process that runs your entrypoint; the browser has its own `globalThis`. Browser code pulls from `tako.sh/client` or `tako.sh/react` instead, and TypeScript will refuse to let you reach for `Tako` from a client-compiled file.

## Try it

`tako typegen` runs automatically during [`tako init`](/docs/cli), [`tako dev`](/docs/development), and [`tako deploy`](/docs/deployment), so most of the time you don't think about it. When you add a secret, channel, or workflow and want types updated without restarting dev, run it directly:

```bash
tako secrets set STRIPE_KEY --env production
tako typegen
# Generated tako.d.ts
```

For Go apps, typegen emits a `tako_secrets.go` with a typed `Secrets` struct — same idea, same compile-time catch. See [the Go SDK post](/blog/the-go-sdk-is-here) for the shape of that side.

Typed globals aren't a new idea. What's new is getting them for secrets, channels, workflows, and runtime env without writing a single type definition by hand.
