---
title: "Named Worker Groups for Tako Workflows"
date: "2026-04-29T04:29"
description: "The Tako SDK and config schema model named workflow groups, but production worker isolation is not wired yet."
image: eef4025ddeaa
---

Workflow queues have one classic failure mode: a slow job clogs the pipe and everything else waits behind it. A 30-second image resize lands in the queue, and the password-reset email that should have gone out in 200ms sits in `pending` while users refresh their inbox.

Named worker groups isolate claims. A workflow with `worker: "email"` is claimed only by a process launched with `TAKO_WORKFLOW_WORKER=email`. Production and `tako dev` start one scale-to-zero lane per declared group, plus the default group.

Worker counts, concurrency, and per-server overrides are still parsed from `tako.toml` but not applied to those lanes. The [workflow reference](/docs/workflows/) and [`tako.toml` reference](/docs/tako-toml/) track the current behavior.

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

Workflows without `worker:` belong to the default group. Production and `tako dev` launch a matching process per group found in the workflows directory.

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

The parser computes the intended precedence chain: built-in defaults, `[workflows]`, `[workflows.<group>]`, then `[servers.<name>.workflows.<group>]`. Isolation uses the declared `worker:` names. Worker counts, concurrency, and per-server overrides are still not applied to those lanes.

Each lane still has a [scale-to-zero](/blog/scale-to-zero-without-containers/) lifecycle. It starts when work becomes runnable and idles out after five minutes.
