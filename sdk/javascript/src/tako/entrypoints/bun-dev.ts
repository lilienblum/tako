#!/usr/bin/env bun
/**
 * Tako Bun Dev Entrypoint — runs HTTP + workflow worker in one process.
 */

import { createEntrypoint } from "../create-entrypoint";
import { drainInProcessWorker, startInProcessWorker } from "../dev-worker";
import { initSecretsFromFd, readViaInheritedFd } from "../secrets";

initSecretsFromFd(readViaInheritedFd);
const { run, host, port, setDraining } = createEntrypoint();

if (import.meta.main) {
  let server: ReturnType<typeof Bun.serve> | undefined;

  void run(async (handleRequest) => {
    server = Bun.serve({ hostname: host, port, fetch: handleRequest });
    queueMicrotask(() => void startInProcessWorker());
    return server.port;
  });

  process.on("SIGTERM", () => {
    setDraining();
    void (async () => {
      await drainInProcessWorker();
      void server?.stop(true);
    })();
  });
}
