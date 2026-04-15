/**
 * WorkflowEngine — SDK-facing surface for durable tasks.
 *
 * This module is imported by **two processes**:
 *
 * 1. **Worker process** (`bunx tako-worker`, etc.) — uses
 *    `discover/register/startWorker/drain`. All DB ops go over RPC to
 *    tako-server via `WorkflowsClient`.
 *
 * 2. **HTTP app process** — `enqueue` delegates to the same RPC client.
 *    The SDK never opens SQLite; tako-server owns the queue file.
 *
 * The same singleton supports both — callers just invoke the relevant
 * subset.
 */

import { discoverWorkflows } from "./discovery";
import { WorkflowsClient } from "./rpc-client";
import type { WorkflowConfig } from "./types";
import type { RunId } from "./types";
import { Worker, type RegisteredWorkflow, type WorkflowHandler } from "./worker";

interface Registration {
  handler: WorkflowHandler;
  config: WorkflowConfig;
}

export interface EnqueueOptions {
  runAt?: Date;
  maxAttempts?: number;
  uniqueKey?: string | null;
}

export class WorkflowEngine {
  private client: WorkflowsClient | null = null;
  private worker: Worker | null = null;
  private workerId = "";
  private configuredFlag = false;
  private readonly registrations = new Map<string, Registration>();

  /** True once configure() has succeeded (worker process only). */
  get configured(): boolean {
    return this.configuredFlag;
  }

  /** The workflow names that have been registered (worker process only). */
  get registeredNames(): string[] {
    return Array.from(this.registrations.keys());
  }

  /** Worker-process setup. Attaches the RPC client + worker identity. */
  configure(opts: { client: WorkflowsClient; workerId: string }): void {
    if (this.configuredFlag) throw new Error("WorkflowEngine already configured");
    this.client = opts.client;
    this.workerId = opts.workerId;
    this.configuredFlag = true;
  }

  /**
   * HTTP-process setup. Explicitly attach a client (tests inject a mock).
   * If not called, the engine lazily tries `WorkflowsClient.fromEnv()` on
   * first enqueue.
   */
  setClient(client: WorkflowsClient | null): void {
    this.client = client;
  }

  register(name: string, handler: WorkflowHandler, config: WorkflowConfig = {}): void {
    if (this.registrations.has(name)) {
      throw new Error(`workflow '${name}' is already registered`);
    }
    this.registrations.set(name, { handler, config });
  }

  /**
   * Scan `dir` for workflow files and register each one by filename (without
   * extension). Re-exports: `default` is the handler; `schedule`,
   * `maxAttempts`, `concurrency`, `timeoutMs` are optional named exports
   * that flow into WorkflowConfig.
   */
  async discover(dir: string): Promise<number> {
    const found = await discoverWorkflows(dir);
    for (const entry of found) {
      this.register(entry.name, entry.handler, entry.config);
    }
    return found.length;
  }

  /**
   * Enqueue a task. In the HTTP process this goes through the per-app
   * enqueue socket (tako-server writes to SQLite). In the worker process
   * (where a backend is configured) it still goes through the RPC path —
   * the worker doesn't self-enqueue via its own DB handle, which keeps
   * server ownership of cron-dedup idempotent.
   */
  async enqueue(name: string, payload: unknown, opts: EnqueueOptions = {}): Promise<RunId> {
    const client = this.resolveClient();
    const effectiveOpts: EnqueueOptions = { ...opts };
    if (effectiveOpts.maxAttempts === undefined) {
      const reg = this.registrations.get(name);
      if (reg?.config.maxAttempts !== undefined) {
        effectiveOpts.maxAttempts = reg.config.maxAttempts;
      }
    }
    const result = await client.enqueue(name, payload, effectiveOpts);
    return result.id;
  }

  /** Deliver an event payload to every parked waitFor matching `eventName`. */
  async signal(eventName: string, payload?: unknown): Promise<number> {
    return this.resolveClient().signal(eventName, payload ?? null);
  }

  private resolveClient(): WorkflowsClient {
    if (!this.client) {
      this.client = WorkflowsClient.fromEnv();
    }
    if (!this.client) {
      throw new Error(
        "Task engine has no RPC client. Set TAKO_ENQUEUE_SOCKET or call setClient().",
      );
    }
    return this.client;
  }

  start(): void {
    this.startWorker({});
  }

  /**
   * Worker-process start with runtime-provided concurrency / idle timeout.
   * Called by the worker entrypoint bootstrap; user code normally uses
   * `start()` which accepts no arguments.
   */
  startWorker(opts: { concurrency?: number; idleTimeoutMs?: number }): void {
    if (this.worker) return;
    const client = this.resolveClient();
    const registry = new Map<string, RegisteredWorkflow>();
    for (const [name, reg] of this.registrations) {
      const entry: RegisteredWorkflow = { handler: reg.handler };
      if (reg.config.maxAttempts !== undefined || reg.config.backoff !== undefined) {
        entry.retry = {};
        if (reg.config.maxAttempts !== undefined) entry.retry.maxAttempts = reg.config.maxAttempts;
        if (reg.config.backoff !== undefined) entry.retry.backoff = reg.config.backoff;
      }
      registry.set(name, entry);
    }
    this.worker = new Worker({
      client,
      registry,
      workerId: this.workerId,
      ...(opts.concurrency !== undefined && { concurrency: opts.concurrency }),
      ...(opts.idleTimeoutMs !== undefined && { idleTimeoutMs: opts.idleTimeoutMs }),
    });
    this.worker.start();
  }

  /** True if the running worker exited because it went idle. */
  get workerIdled(): boolean {
    return this.worker?.idled ?? false;
  }

  async drain(): Promise<void> {
    if (this.worker) {
      await this.worker.drain();
      this.worker = null;
    }
  }

  running(): number {
    return this.worker?.runningCount ?? 0;
  }

  /** Gather registered cron schedules for `RegisterSchedules`. */
  collectSchedules(): Array<{ name: string; cron: string }> {
    const out: Array<{ name: string; cron: string }> = [];
    for (const [name, reg] of this.registrations) {
      if (reg.config.schedule) {
        out.push({ name, cron: reg.config.schedule });
      }
    }
    return out;
  }

  /** Test-only: reset all state. */
  _reset(): void {
    this.client = null;
    this.worker = null;
    this.workerId = "";
    this.configuredFlag = false;
    this.registrations.clear();
  }
}

/** Singleton exported on the global Tako object. */
export const workflowsEngine = new WorkflowEngine();
