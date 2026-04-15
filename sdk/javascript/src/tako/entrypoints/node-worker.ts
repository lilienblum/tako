#!/usr/bin/env node
/**
 * Tako Node Worker Entrypoint — run via `npx tako-worker-node`.
 */

import { initSecretsFromFd, readViaInheritedFd } from "../secrets";
import { installTakoGlobal } from "../../tako";
import { bootstrapWorker } from "../../workflows/bootstrap";
import { workflowsEngine } from "../../workflows/engine";

async function main(): Promise<void> {
  initSecretsFromFd(readViaInheritedFd);
  installTakoGlobal();
  const result = await bootstrapWorker();
  if (!result.started) {
    console.error(`tako-worker: not started (${result.reason ?? "unknown"})`);
    process.exit(result.reason === "no workflows discovered" ? 0 : 1);
  }
  console.error(`tako-worker: running ${result.workflowCount} workflow(s)`);

  let shuttingDown = false;
  const shutdown = async (signal: string): Promise<void> => {
    if (shuttingDown) return;
    shuttingDown = true;
    console.error(`tako-worker: received ${signal}, draining`);
    await workflowsEngine.drain();
    process.exit(0);
  };
  process.on("SIGTERM", () => void shutdown("SIGTERM"));
  process.on("SIGINT", () => void shutdown("SIGINT"));

  const idleCheck = setInterval(() => {
    if (workflowsEngine.workerIdled && !shuttingDown) {
      clearInterval(idleCheck);
      void shutdown("idle");
    }
  }, 1_000);
}

void main();
