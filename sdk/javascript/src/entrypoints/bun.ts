#!/usr/bin/env bun
/**
 * Tako Bun Entrypoint — run via `bunx tako-bun <main>`
 */

import { createEntrypoint } from "../create-entrypoint";

const { run, appSocketPath, port, setDraining } = createEntrypoint();

if (import.meta.main) {
  void run((handleRequest) => {
    if (appSocketPath) {
      Bun.serve({ unix: appSocketPath, fetch: handleRequest });
      console.log(`Application listening on ${appSocketPath}`);
    } else {
      Bun.serve({ port, fetch: handleRequest });
      console.log(`Application listening on http://localhost:${port}`);
    }
  });

  process.on("SIGTERM", () => {
    setDraining();
  });
}
