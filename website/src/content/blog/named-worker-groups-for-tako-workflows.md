---
title: "Named Worker Groups for Tako Workflows"
date: "2026-04-29T04:29"
description: "The Tako SDK and config schema model named workflow groups, but production worker isolation is not wired yet."
image: eef4025ddeaa
---

Workflow queues have one classic failure mode: a slow job clogs the pipe and everything else waits behind it. A 30-second image resize lands in the queue, and the password-reset email that should have gone out in 200ms sits in `pending` while users refresh their inbox.

Named worker groups are the intended answer, but they are not a shipped production guarantee yet. The JavaScript SDK records group metadata, discovery can filter on it, and `tako.toml` parses group settings. The production supervisor still starts one shared scale-to-zero lane per app with fixed runtime settings.

This page describes the modeled API and the remaining boundary. Do not rely on group isolation, worker counts, concurrency settings, or per-server workflow overrides in production today. The [workflow reference](/docs/workflows/) and [`tako.toml` reference](/docs/tako-toml/) track the current behavior.

## The modeled API

A workflow can carry a group name:

```ts
// src/workflows/process-image.ts
import { defineWorkflow } from "tako.sh";

export default defineWorkflow<{ key: string }>("process-image", {
  worker: "media",
  retries: 4,
  handler: async (payload, ctx) => {
    const buf = await ctx.run("download", () => s3.get(payload.key));
    await ctx.run("resize", () => sharp(buf).resize(1024).toBuffer());
    await ctx.run("upload", () => s3.put(`thumb/${payload.key}`, buf));
  },
});
```

Workflows without `worker:` belong to the SDK's default discovery group. This metadata is useful to the workflow loader and its tests, but the production supervisor does not currently launch a matching process per group.

## The parsed configuration

The config schema accepts base settings, named groups, and per-server overrides:

```toml
[workflows]
workers = 0          # modeled base worker count
concurrency = 10

[workflows.email]
workers = 1          # modeled group override
concurrency = 20

[workflows.media]
workers = 2
concurrency = 4

[servers.lax.workflows.media]
workers = 4          # modeled per-server override
```

The parser computes the intended precedence chain: built-in defaults, `[workflows]`, `[workflows.<group>]`, then `[servers.<name>.workflows.<group>]`. Production worker supervision does not consume that resolved configuration yet, so changing these values does not create separate pools or tune the current lane.

## What still needs to ship

The missing piece is production orchestration. A complete implementation must resolve each configured group, launch the requested number of worker processes, pass the group and concurrency into each process, wake the right lane for runnable work, and apply per-server overrides. Until then, all workflows share one production worker process.

```d2
direction: right

ent1: "enqueue send-email" {style.fill: "#9BC4B6"; style.font-size: 14}
ent2: "enqueue process-image" {style.fill: "#9BC4B6"; style.font-size: 14}
server: "tako-server\ncurrent supervisor" {style.fill: "#E88783"; style.font-size: 14}
shared: "shared worker lane\nscale-to-zero" {style.fill: "#FFF9F4"; style.stroke: "#2F2A44"; style.font-size: 14}
future: "future group-aware\nsupervision" {style.fill: "#FFF9F4"; style.stroke: "#2F2A44"; style.font-size: 14}

ent1 -> server
ent2 -> server
server -> shared: "today"
server -> future: "not wired"
```

The current shared lane still has a [scale-to-zero](/blog/scale-to-zero-without-containers/) lifecycle. It starts when work becomes runnable and idles out after five minutes. If you need hard isolation today, deploy the workloads as separate apps or use another process manager. Treat named groups as reserved configuration until the production supervisor is group-aware.
