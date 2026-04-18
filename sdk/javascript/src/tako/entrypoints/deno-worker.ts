#!/usr/bin/env -S deno run --allow-env --allow-read --allow-net
/**
 * Tako Deno Worker Entrypoint.
 */

import { installConsoleBridge } from "../../console-bridge";
import { installErrorHooks } from "../../error-hooks";
import { initBootstrapFromFd, readViaProcSelfFd } from "../secrets";
import { installTakoGlobal } from "../../tako";
import { bootstrapWorker } from "../../workflows/bootstrap";
import { workflowsEngine } from "../../workflows/engine";

declare const Deno: {
  addSignalListener(signal: string, cb: () => void): void;
  exit(code?: number): never;
};

installErrorHooks("worker");
installConsoleBridge("worker");

async function main(): Promise<void> {
  initBootstrapFromFd(readViaProcSelfFd);
  installTakoGlobal();
  const result = await bootstrapWorker();
  const exit: (code?: number) => never =
    typeof Deno !== "undefined"
      ? (code?: number) => Deno.exit(code)
      : (code?: number) => process.exit(code);

  if (!result.started) {
    console.error(`tako-worker: not started (${result.reason ?? "unknown"})`);
    exit(result.reason === "no workflows discovered" ? 0 : 1);
  }
  console.log(`tako-worker: running ${result.workflowCount} workflow(s)`);

  let shuttingDown = false;
  const shutdown = async (signal: string): Promise<void> => {
    if (shuttingDown) return;
    shuttingDown = true;
    console.log(`tako-worker: received ${signal}, draining`);
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
