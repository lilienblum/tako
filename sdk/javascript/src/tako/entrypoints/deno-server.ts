#!/usr/bin/env -S deno run --allow-net --allow-env --allow-read --allow-write --node-modules-dir=auto
/**
 * Tako Deno Entrypoint — run via `deno run npm:tako-deno <main>`.
 *
 * Deno's node compat can't read/write inherited fds (3 for secrets, 4 for
 * readiness) the way Bun and Node can. We inject overrides that open those
 * fds via `/proc/self/fd/N` (Linux) or `/dev/fd/N` (macOS) so the shared
 * `createEntrypoint` flow works unchanged.
 */

import { createEntrypoint } from "../create-entrypoint";
import { installStdioBridge } from "../stdio-bridge";
import { writeViaProcSelfFd } from "../readiness";
import { initBootstrapFromFd, readViaProcSelfFd } from "../secrets-fd";

installStdioBridge("app");
initBootstrapFromFd(readViaProcSelfFd);
const { run, host, port, setDraining } = createEntrypoint({
  signalReadyPortOnFd: writeViaProcSelfFd,
});

void run((handleRequest) => {
  // @ts-ignore - Deno global
  const server = Deno.serve({ hostname: host, port }, handleRequest);
  // @ts-ignore - Deno server addr
  const actualPort: number = server.addr?.port ?? port;

  // @ts-ignore - Deno global
  Deno.addSignalListener?.("SIGTERM", () => {
    setDraining();
    void server.shutdown();
  });

  return actualPort;
});
