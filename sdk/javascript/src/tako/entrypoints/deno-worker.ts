#!/usr/bin/env -S deno run --allow-env --allow-read --allow-net
/**
 * Tako Deno Worker Entrypoint.
 */

import { initSecretsFromFd, readViaProcSelfFd } from "../secrets";
import { installTakoGlobal } from "../../tako";
import { bootstrapWorker } from "../../workflows/bootstrap";
import { workflowsEngine } from "../../workflows/engine";

declare const Deno: {
  addSignalListener(signal: string, cb: () => void): void;
  exit(code?: number): never;
};

async function main(): Promise<void> {
  initSecretsFromFd(readViaProcSelfFd);
  installTakoGlobal();
  const result = await bootstrapWorker();
  const exit = typeof Deno !== "undefined" ? Deno.exit : process.exit;

  if (!result.started) {
    console.error(`tako-worker: not started (${result.reason ?? "unknown"})`);
    exit(result.reason === "no workflows discovered" ? 0 : 1);
    return;
  }
  console.error(`tako-worker: running ${result.workflowCount} workflow(s)`);

  let shuttingDown = false;
  const shutdown = async (signal: string): Promise<void> => {
    if (shuttingDown) return;
    shuttingDown = true;
    console.error(`tako-worker: received ${signal}, draining`);
    await workflowsEngine.drain();
    exit(0);
  };

  if (typeof Deno !== "undefined") {
    Deno.addSignalListener("SIGTERM", () => void shutdown("SIGTERM"));
    Deno.addSignalListener("SIGINT", () => void shutdown("SIGINT"));
  } else {
    process.on("SIGTERM", () => void shutdown("SIGTERM"));
    process.on("SIGINT", () => void shutdown("SIGINT"));
  }

  const idleCheck = setInterval(() => {
    if (workflowsEngine.workerIdled && !shuttingDown) {
      clearInterval(idleCheck);
      void shutdown("idle");
    }
  }, 1_000);
}

void main();
