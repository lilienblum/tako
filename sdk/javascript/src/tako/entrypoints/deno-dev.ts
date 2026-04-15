#!/usr/bin/env -S deno run --allow-net --allow-env --allow-read --allow-write --node-modules-dir=auto
/**
 * Tako Deno Dev Entrypoint — HTTP + workflow worker in one process.
 */

import { createEntrypoint } from "../create-entrypoint";
import { drainInProcessWorker, startInProcessWorker } from "../dev-worker";
import { writeViaProcSelfFd } from "../readiness";
import { initSecretsFromFd, readViaProcSelfFd } from "../secrets";

initSecretsFromFd(readViaProcSelfFd);
const { run, host, port, setDraining } = createEntrypoint({
  signalReadyPortOnFd: writeViaProcSelfFd,
});

void run((handleRequest) => {
  // @ts-ignore - Deno global
  const server = Deno.serve({ hostname: host, port }, handleRequest);
  // @ts-ignore - Deno server addr
  const actualPort: number = server.addr?.port ?? port;

  queueMicrotask(() => void startInProcessWorker());

  // @ts-ignore - Deno global
  Deno.addSignalListener?.("SIGTERM", () => {
    setDraining();
    void (async () => {
      await drainInProcessWorker();
      void server.shutdown();
    })();
  });

  return actualPort;
});
