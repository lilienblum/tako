/**
 * Worker loop — claims one run at a time, executes its handler with a
 * checkpointed step API, heartbeats the lease, and finalizes the run via
 * `complete` / `cancel` / `fail` / `defer` / `wait_for_event` based on the
 * outcome.
 *
 * Sentinel exceptions (BailSignal/FailSignal/DeferSignal/WaitSignal) drive
 * the non-retry termination paths cleanly.
 *
 * Start one Worker per app instance. `drain()` stops claiming and awaits
 * any in-flight run for the platform drain hook.
 */

import { expBackoffMs } from "./backoff";
import type { WorkflowsClient } from "./rpc-client";
import {
  BailSignal,
  createStepAPI,
  DeferSignal,
  FailSignal,
  WaitSignal,
  type StepRunOptions,
  type StepWaitOptions,
} from "./step";
import type { Run, StepState } from "./types";

export type WorkflowHandler<P = unknown, R = unknown> = (
  payload: P,
  ctx: WorkflowContext,
) => Promise<R> | R;

export interface WorkflowContext {
  readonly runId: string;
  readonly workflowName: string;
  readonly attempts: number;
  run<T>(name: string, fn: () => Promise<T> | T, opts?: StepRunOptions): Promise<T>;
  sleep(name: string, durationMs: number): Promise<void>;
  waitFor<T = unknown>(name: string, opts?: StepWaitOptions): Promise<T | null>;
  /** End the run cleanly as `cancelled` (no retries). For "this work
   *  isn't needed anymore" cases. To exit successfully early, just
   *  `return` from the handler. */
  bail(reason?: string): never;
  /** End the run as `dead` immediately (no retries). For permanent
   *  errors that won't get better with retries. */
  fail(error: Error | string): never;
}

interface WorkflowRetryConfig {
  /** Run-level retry budget (default 3). */
  maxAttempts?: number;
  /** Run-level backoff between failed attempts. */
  backoff?: { base?: number; max?: number };
}

export interface WorkerOptions {
  client: WorkflowsClient;
  registry: Map<string, RegisteredWorkflow>;
  workerId: string;
  leaseMs?: number;
  heartbeatIntervalMs?: number;
  pollIntervalMs?: number;
  baseBackoffMs?: number;
  maxBackoffMs?: number;
  /** Scale-to-zero: exit poll loop after this many ms with no claim. */
  idleTimeoutMs?: number;
  /** Reserved for multi-slot parallelism; v1 ignores. */
  concurrency?: number;
}

export interface RegisteredWorkflow {
  handler: WorkflowHandler;
  retry?: WorkflowRetryConfig;
}

const DEFAULTS = {
  leaseMs: 60_000,
  pollIntervalMs: 1_000,
  baseBackoffMs: 1_000,
  maxBackoffMs: 3_600_000,
  idleTimeoutMs: 0,
} as const;

export class Worker {
  private readonly client: WorkflowsClient;
  private readonly registry: Map<string, RegisteredWorkflow>;
  private readonly workerId: string;
  private readonly leaseMs: number;
  private readonly heartbeatIntervalMs: number;
  private readonly pollIntervalMs: number;
  private readonly baseBackoffMs: number;
  private readonly maxBackoffMs: number;
  private readonly idleTimeoutMs: number;

  private draining = false;
  private idledOut = false;
  private lastClaimAt = 0;
  private readonly inFlight = new Set<Promise<void>>();
  private loopPromise: Promise<void> | null = null;

  constructor(opts: WorkerOptions) {
    this.client = opts.client;
    this.registry = opts.registry;
    this.workerId = opts.workerId;
    this.leaseMs = opts.leaseMs ?? DEFAULTS.leaseMs;
    this.heartbeatIntervalMs = opts.heartbeatIntervalMs ?? Math.floor(this.leaseMs / 3);
    this.pollIntervalMs = opts.pollIntervalMs ?? DEFAULTS.pollIntervalMs;
    this.baseBackoffMs = opts.baseBackoffMs ?? DEFAULTS.baseBackoffMs;
    this.maxBackoffMs = opts.maxBackoffMs ?? DEFAULTS.maxBackoffMs;
    this.idleTimeoutMs = opts.idleTimeoutMs ?? DEFAULTS.idleTimeoutMs;
    this.lastClaimAt = Date.now();
  }

  get idled(): boolean {
    return this.idledOut;
  }

  async processOnce(): Promise<boolean> {
    if (this.draining) return false;
    const names = Array.from(this.registry.keys());
    const run = await this.client.claim(this.workerId, names, this.leaseMs);
    if (!run) return false;
    this.lastClaimAt = Date.now();

    const work = this.execute(run);
    this.inFlight.add(work);
    try {
      await work;
    } finally {
      this.inFlight.delete(work);
    }
    return true;
  }

  start(): void {
    if (this.loopPromise) return;
    this.loopPromise = this.runLoop();
  }

  async drain(): Promise<void> {
    this.draining = true;
    if (this.loopPromise) {
      await this.loopPromise;
      this.loopPromise = null;
    }
    await Promise.allSettled(Array.from(this.inFlight));
  }

  get runningCount(): number {
    return this.inFlight.size;
  }

  private async runLoop(): Promise<void> {
    while (!this.draining) {
      const did = await this.processOnce().catch(() => false);
      if (!did && !this.draining) {
        if (this.idleTimeoutMs > 0 && Date.now() - this.lastClaimAt >= this.idleTimeoutMs) {
          this.idledOut = true;
          this.draining = true;
          break;
        }
        await new Promise((r) => setTimeout(r, this.pollIntervalMs));
      }
    }
  }

  private async execute(run: Run): Promise<void> {
    const reg = this.registry.get(run.name);
    if (!reg) {
      await this.client.fail(
        run.id,
        this.workerId,
        `no handler registered for '${run.name}'`,
        null,
        true,
      );
      return;
    }

    const stepState: StepState = { ...run.stepState };
    const ctx: WorkflowContext = {
      runId: run.id,
      workflowName: run.name,
      attempts: run.attempts,
      ...createStepAPI(this.client, run.id, this.workerId, stepState),
      bail: (reason?: string): never => {
        throw new BailSignal(reason);
      },
      fail: (error: Error | string): never => {
        const e = error instanceof Error ? error : new Error(error);
        throw new FailSignal(e);
      },
    };

    let heartbeatTimer: ReturnType<typeof setInterval> | null = null;
    if (this.heartbeatIntervalMs > 0) {
      heartbeatTimer = setInterval(() => {
        this.client.heartbeat(run.id, this.workerId, this.leaseMs).catch(() => {});
      }, this.heartbeatIntervalMs);
    }

    try {
      await reg.handler(run.payload, ctx);
      await this.client.complete(run.id, this.workerId);
    } catch (err) {
      if (err instanceof BailSignal) {
        await this.client.cancel(run.id, this.workerId, err.reason ?? null);
        return;
      }
      if (err instanceof FailSignal) {
        await this.client.fail(run.id, this.workerId, err.error.message, null, true);
        return;
      }
      if (err instanceof DeferSignal) {
        await this.client.defer(run.id, this.workerId, err.wakeAt);
        return;
      }
      if (err instanceof WaitSignal) {
        await this.client.waitForEvent(
          run.id,
          this.workerId,
          err.stepName,
          err.eventName,
          err.timeoutAt,
        );
        return;
      }

      // Regular error → run-level retry path.
      const message = err instanceof Error ? err.message : String(err);
      const maxAttempts = reg.retry?.maxAttempts ?? run.retries + 1;
      const finalize = run.attempts >= maxAttempts;
      const base = reg.retry?.backoff?.base ?? this.baseBackoffMs;
      const max = reg.retry?.backoff?.max ?? this.maxBackoffMs;
      const nextRunAt = finalize
        ? null
        : new Date(Date.now() + expBackoffMs(run.attempts, base, max));
      await this.client.fail(run.id, this.workerId, message, nextRunAt, finalize);
    } finally {
      if (heartbeatTimer) clearInterval(heartbeatTimer);
    }
  }
}
