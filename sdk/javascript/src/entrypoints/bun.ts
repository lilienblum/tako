#!/usr/bin/env bun
/**
 * Tako Bun Entrypoint — run via `bunx tako-bun <main>`
 */

import { createEntrypoint } from "../create-entrypoint";

const { run, host, port, setDraining } = createEntrypoint();

if (import.meta.main) {
  void run((handleRequest) => {
    Bun.serve({ hostname: host, port, fetch: handleRequest });
    console.log(`Application listening on http://${host}:${port}`);
  });

  process.on("SIGTERM", () => {
    setDraining();
  });
}
