#!/usr/bin/env node
/**
 * Tako Node Worker Entrypoint — run via `npx tako-worker-node`.
 */

import { installConsoleBridge } from "../../console-bridge";
import { installErrorHooks } from "../../error-hooks";
import { initBootstrapFromFd, readViaInheritedFd } from "../secrets";
import { installTakoGlobal } from "../../tako";
import { bootstrapWorker } from "../../workflows/bootstrap";
import { workflowsEngine } from "../../workflows/engine";

installErrorHooks("worker");
installConsoleBridge("worker");

async function main(): Promise<void> {
  initBootstrapFromFd(readViaInheritedFd);
  installTakoGlobal();
  const result = await bootstrapWorker();
  if (!result.started) {
    console.error(`tako-worker: not started (${result.reason ?? "unknown"})`);
    process.exit(result.reason === "no workflows discovered" ? 0 : 1);
  }
  console.log(`tako-worker: running ${result.workflowCount} workflow(s)`);

  let shuttingDown = false;
  const shutdown = async (signal: string): Promise<void> => {
    if (shuttingDown) return;
    shuttingDown = true;
    console.log(`tako-worker: received ${signal}, draining`);
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
