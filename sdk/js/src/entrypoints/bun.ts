/**
 * Tako Bun Entrypoint
 *
 * Runs user apps under Bun with Tako internal endpoints.
 * Usage: bun run entrypoints/bun.ts <app-path>
 */

import { createEntrypoint } from "../create-entrypoint";

const { run, appSocketPath, port, setDraining } = createEntrypoint();

if (import.meta.main) {
  run((handleRequest) => {
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
