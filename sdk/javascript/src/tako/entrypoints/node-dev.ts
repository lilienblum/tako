#!/usr/bin/env node
/**
 * Tako Node.js Dev Entrypoint — HTTP + workflow worker in one process.
 */

import { createEntrypoint } from "../create-entrypoint";
import { drainInProcessWorker, startInProcessWorker } from "../dev-worker";
import { startNodeServer } from "../node-http";
import { initSecretsFromFd, readViaInheritedFd } from "../secrets";

initSecretsFromFd(readViaInheritedFd);
const { run, host, port, setDraining } = createEntrypoint();

void run(async (handleRequest) => {
  const { actualPort, close } = await startNodeServer(host, port, handleRequest);
  queueMicrotask(() => void startInProcessWorker());

  process.on("SIGTERM", () => {
    setDraining();
    void (async () => {
      await drainInProcessWorker();
      close();
    })();
  });

  return actualPort;
});
