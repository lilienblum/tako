---
layout: ../../layouts/DocsLayout.astro
title: Tako Docs - Framework Guides
heading: Framework Guides
current: frameworks
---

# Framework Guides

Framework adapter examples and runtime integration notes.

## Built-in adapters

### Bun

Use the Bun adapter from `tako.sh`:

```ts
import { serve } from "tako.sh/bun";

serve({
  fetch() {
    return new Response("Hello from Bun + Tako");
  },
});
```
