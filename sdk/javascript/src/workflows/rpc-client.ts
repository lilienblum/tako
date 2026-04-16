/**
 * WorkflowsClient — single client for all workflow RPCs.
 *
 * Runs in the HTTP app process (for `Tako.workflows.enqueue` and
 * `Tako.workflows.signal`) and in the worker process (for claim, heartbeat,
 * saveStep, complete, cancel, fail, defer, waitForEvent). The SDK never
 * touches SQLite — tako-server owns the queue file; everything reaches it
 * via the shared unix socket at `TAKO_WORKFLOW_SOCKET`.
 *
 * Every command carries the app name (from `TAKO_APP_NAME`), so one
 * tako-server socket can route for every deployed app.
 */

import { createConnection } from "node:net";
import type { EnqueueOptions } from "./engine";
import type { Run, RunId, RunStatus, StepState } from "./types";

const WORKFLOW_SOCKET_ENV = "TAKO_WORKFLOW_SOCKET";
const APP_NAME_ENV = "TAKO_APP_NAME";

export class WorkflowsError extends Error {
  constructor(message: string) {
    super(message);
    this.name = "WorkflowsError";
  }
}

export interface EnqueueResult {
  id: RunId;
  deduplicated: boolean;
}

interface RpcResponse {
  status: "ok" | "error";
  data?: unknown;
  message?: string;
}

export class WorkflowsClient {
  private readonly socketPath: string;
  private readonly app: string;

  constructor(socketPath: string, app: string) {
    this.socketPath = socketPath;
    this.app = app;
  }

  /**
   * Build a client from env vars set by tako-server when spawning the
   * app/worker process. Returns null when the env vars are absent (outside
   * of a Tako-managed process) — callers should fall back or error.
   */
  static fromEnv(): WorkflowsClient | null {
    const path = process.env[WORKFLOW_SOCKET_ENV];
    const app = process.env[APP_NAME_ENV];
    if (!path || !app) return null;
    return new WorkflowsClient(path, app);
  }

  /** The app name this client sends on every RPC. */
  get appName(): string {
    return this.app;
  }

  // --- Enqueue / signal: usable from any process ---

  async enqueue(name: string, payload: unknown, opts: EnqueueOptions = {}): Promise<EnqueueResult> {
    const wire: Record<string, unknown> = {};
    if (opts.runAt !== undefined) wire["run_at_ms"] = opts.runAt.getTime();
    if (opts.retries !== undefined) wire["max_attempts"] = opts.retries + 1;
    if (opts.uniqueKey !== undefined && opts.uniqueKey !== null) {
      wire["unique_key"] = opts.uniqueKey;
    }
    const data = await this.call({
      command: "enqueue_run",
      app: this.app,
      name,
      payload: payload ?? null,
      opts: wire,
    });
    const d = data as { id: string; deduplicated: boolean } | null;
    if (!d || typeof d.id !== "string") {
      throw new WorkflowsError("malformed enqueue response");
    }
    return { id: d.id, deduplicated: Boolean(d.deduplicated) };
  }

  async signal(eventName: string, payload: unknown): Promise<number> {
    const data = await this.call({
      command: "signal",
      app: this.app,
      event_name: eventName,
      payload: payload ?? null,
    });
    const d = data as { woken?: number } | null;
    return d?.woken ?? 0;
  }

  // --- Worker-only: registration + run lifecycle ---

  async registerSchedules(schedules: Array<{ name: string; cron: string }>): Promise<void> {
    await this.call({ command: "register_schedules", app: this.app, schedules });
  }

  async claim(workerId: string, names: string[], leaseMs: number): Promise<Run | null> {
    const data = await this.call({
      command: "claim_run",
      app: this.app,
      worker_id: workerId,
      names,
      lease_ms: leaseMs,
    });
    if (data === null || data === undefined) return null;
    return rawToRun(data as RawRun);
  }

  async heartbeat(id: RunId, workerId: string, leaseMs: number): Promise<void> {
    await this.call({
      command: "heartbeat_run",
      app: this.app,
      id,
      worker_id: workerId,
      lease_ms: leaseMs,
    });
  }

  async saveStep(id: RunId, workerId: string, stepName: string, result: unknown): Promise<void> {
    await this.call({
      command: "save_step",
      app: this.app,
      id,
      worker_id: workerId,
      step_name: stepName,
      result: result ?? null,
    });
  }

  async complete(id: RunId, workerId: string): Promise<void> {
    await this.call({ command: "complete_run", app: this.app, id, worker_id: workerId });
  }

  async cancel(id: RunId, workerId: string, reason?: string | null): Promise<void> {
    await this.call({
      command: "cancel_run",
      app: this.app,
      id,
      worker_id: workerId,
      reason: reason ?? null,
    });
  }

  async fail(
    id: RunId,
    workerId: string,
    error: string,
    nextRunAt: Date | null,
    finalize: boolean,
  ): Promise<void> {
    await this.call({
      command: "fail_run",
      app: this.app,
      id,
      worker_id: workerId,
      error,
      next_run_at_ms: nextRunAt ? nextRunAt.getTime() : null,
      finalize,
    });
  }

  async defer(id: RunId, workerId: string, wakeAt: Date | null): Promise<void> {
    await this.call({
      command: "defer_run",
      app: this.app,
      id,
      worker_id: workerId,
      wake_at_ms: wakeAt ? wakeAt.getTime() : null,
    });
  }

  async waitForEvent(
    id: RunId,
    workerId: string,
    stepName: string,
    eventName: string,
    timeoutAt: Date | null,
  ): Promise<void> {
    await this.call({
      command: "wait_for_event",
      app: this.app,
      id,
      worker_id: workerId,
      step_name: stepName,
      event_name: eventName,
      timeout_at_ms: timeoutAt ? timeoutAt.getTime() : null,
    });
  }

  // --- Internal ---

  private async call(cmd: unknown): Promise<unknown> {
    const resp = await this.roundTrip(cmd);
    if (resp.status === "error") {
      throw new WorkflowsError(resp.message ?? "rpc failed");
    }
    return resp.data ?? null;
  }

  private roundTrip(cmd: unknown): Promise<RpcResponse> {
    return new Promise<RpcResponse>((resolve, reject) => {
      const socket = createConnection(this.socketPath);
      let buf = "";
      let settled = false;

      const settle = (fn: () => void): void => {
        if (settled) return;
        settled = true;
        socket.removeAllListeners();
        socket.destroy();
        fn();
      };

      socket.once("error", (err) => settle(() => reject(err)));
      socket.once("connect", () => {
        socket.write(`${JSON.stringify(cmd)}\n`);
      });
      socket.on("data", (chunk: Buffer) => {
        buf += chunk.toString("utf8");
        const nl = buf.indexOf("\n");
        if (nl === -1) return;
        const line = buf.slice(0, nl);
        try {
          settle(() => resolve(JSON.parse(line) as RpcResponse));
        } catch (err) {
          settle(() => reject(new WorkflowsError(`invalid JSON from server: ${String(err)}`)));
        }
      });
      socket.once("end", () => {
        settle(() => reject(new WorkflowsError("socket closed without response")));
      });
      socket.setTimeout(30_000, () => {
        settle(() => reject(new WorkflowsError("rpc timed out")));
      });
    });
  }
}

interface RawRun {
  id: string;
  name: string;
  payload: unknown;
  status: string;
  attempts: number;
  max_attempts: number;
  run_at_ms: number;
  step_state: StepState;
}

function rawToRun(raw: RawRun): Run {
  return {
    id: raw.id,
    name: raw.name,
    payload: raw.payload,
    status: raw.status as RunStatus,
    attempts: raw.attempts,
    retries: raw.max_attempts - 1,
    runAt: raw.run_at_ms,
    leaseUntil: null,
    workerId: null,
    lastError: null,
    stepState: raw.step_state ?? {},
    createdAt: 0,
    uniqueKey: null,
  };
}
