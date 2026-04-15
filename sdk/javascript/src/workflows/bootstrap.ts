/**
 * Worker-process bootstrap.
 *
 * Called from each runtime's worker entrypoint (`bun-worker.ts`,
 * `node-worker.ts`, `deno-worker.ts`). Reads env vars set by tako-server,
 * attaches the RPC client, discovers `workflows/`, and starts the worker
 * loop. The SDK never opens SQLite — tako-server owns the queue DB and
 * serves all state via the per-app enqueue socket.
 *
 * Env vars (set by tako-server when it spawns the worker):
 *   TAKO_ENQUEUE_SOCKET        — path to the per-app unix socket
 *   TAKO_WORKER_CONCURRENCY    — max parallel tasks per worker (default 10)
 *   TAKO_WORKER_IDLE_TIMEOUT_MS — scale-to-zero idle timeout; 0 = never
 *   TAKO_INSTANCE_ID           — unique id used as the claim leaseholder
 */

import { join } from "node:path";
import { workflowsEngine } from "./engine";
import { WorkflowsClient } from "./rpc-client";

export interface WorkerBootstrapOptions {
  /** Directory containing the `workflows/` subdir. Defaults to `process.cwd()`. */
  appDir?: string;
}

export interface WorkerBootstrapResult {
  started: boolean;
  reason?: string;
  workflowCount: number;
}

const WORKFLOWS_DIRNAME = "workflows";

export async function bootstrapWorker(
  opts: WorkerBootstrapOptions = {},
): Promise<WorkerBootstrapResult> {
  const appDir = opts.appDir ?? process.cwd();
  const client = WorkflowsClient.fromEnv();
  if (!client) {
    return { started: false, reason: "TAKO_ENQUEUE_SOCKET is not set", workflowCount: 0 };
  }

  const concurrency = parseIntEnv("TAKO_WORKER_CONCURRENCY", 10);
  const idleTimeoutMs = parseIntEnv("TAKO_WORKER_IDLE_TIMEOUT_MS", 0);
  const instanceId = process.env["TAKO_INSTANCE_ID"] ?? `worker-${process.pid}`;

  workflowsEngine.configure({ client, workerId: instanceId });

  const workflowsDir = join(appDir, WORKFLOWS_DIRNAME);
  const count = await workflowsEngine.discover(workflowsDir);
  if (count === 0) {
    return { started: false, reason: "no workflows discovered", workflowCount: 0 };
  }

  // Tell the server about any cron schedules.
  const schedules = workflowsEngine.collectSchedules();
  if (schedules.length > 0) {
    await client.registerSchedules(schedules);
  }

  workflowsEngine.startWorker({ concurrency, idleTimeoutMs });
  return { started: true, workflowCount: count };
}

function parseIntEnv(name: string, fallback: number): number {
  const raw = process.env[name];
  if (!raw) return fallback;
  const n = Number.parseInt(raw, 10);
  return Number.isFinite(n) && n >= 0 ? n : fallback;
}
