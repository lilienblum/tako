/**
 * Tako Deno Entrypoint
 *
 * Runs user apps under Deno with Tako internal endpoints.
 * Usage: deno run --allow-net --allow-env --allow-read entrypoints/deno.ts <app-path>
 */

import { createEntrypoint } from "../create-entrypoint";

const { run, appSocketPath, port, setDraining } = createEntrypoint();

void run((handleRequest) => {
  if (appSocketPath) {
    // @ts-ignore - Deno.serve accepts path for unix sockets
    Deno.serve({ path: appSocketPath }, handleRequest);
    console.log(`Application listening on ${appSocketPath}`);
  } else {
    // @ts-ignore - Deno global
    Deno.serve({ port }, handleRequest);
    console.log(`Application listening on http://localhost:${port}`);
  }

  // @ts-ignore - Deno global
  Deno.addSignalListener?.("SIGTERM", () => {
    setDraining();
  });
});
