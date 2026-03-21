#!/usr/bin/env -S deno run --allow-net --allow-env --allow-read --allow-write --node-modules-dir=auto
/**
 * Tako Deno Entrypoint — run via `deno run npm:tako-deno <main>`
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
