#!/usr/bin/env -S deno run --allow-net --allow-env --allow-read --allow-write --node-modules-dir=auto
/**
 * Tako Deno Entrypoint — run via `deno run npm:tako-deno <main>`
 */

import { createEntrypoint } from "../create-entrypoint";

const { run, host, port, setDraining } = createEntrypoint();

void run((handleRequest) => {
  // @ts-ignore - Deno global
  const server = Deno.serve({ hostname: host, port }, handleRequest);
  // @ts-ignore - Deno server addr
  const actualPort: number = server.addr?.port ?? port;
  console.log(`Application listening on http://${host}:${actualPort}`);

  // @ts-ignore - Deno global
  Deno.addSignalListener?.("SIGTERM", () => {
    setDraining();
    void server.shutdown();
  });

  return actualPort;
});
