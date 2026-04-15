/**
 * Step API + workflow control signals.
 *
 * `ctx.step.run(name, fn, opts?)` memoizes fn's result in the run's steps
 * table. On retry, completed steps return their stored value instead of
 * re-executing. Per-step `retries`/`backoff`/`retry: false` options control
 * in-step retry behavior independent of the run-level retry budget.
 *
 * `ctx.step.sleep(name, durationMs)` waits durably. Short sleeps are
 * inline; long sleeps (≥INLINE_SLEEP_THRESHOLD_MS) defer the run via
 * `client.defer` so the worker can release.
 *
 * `ctx.step.waitFor(name, opts?)` parks the run until a matching
 * `Tako.workflows.signal(eventName, payload)` arrives or the timeout fires.
 * Resumption hydrates the event payload as the step's result.
 *
 * **At-least-once contract**: if the worker dies between fn() returning and
 * saveStep persisting, fn re-runs on next claim. Make step bodies
 * idempotent (Stripe idempotency keys, upsert not insert, etc.).
 */

import { expBackoffMs } from "./backoff";
import type { WorkflowsClient } from "./rpc-client";
import type { RunId, StepState } from "./types";

const INLINE_SLEEP_THRESHOLD_MS = 30_000;

export interface StepRunOptions {
  /** In-step retry attempts before propagating. Default 0. */
  retries?: number;
  /** Backoff between in-step retries. */
  backoff?: { base?: number; max?: number };
  /** When true, any throw inside fn fails the run immediately. */
  retry?: false;
}

export interface StepWaitOptions {
  /** Timeout in ms. After this elapses without a matching signal, the
   *  step resolves to `null`. Default: no timeout (parked indefinitely). */
  timeout?: number;
}

export interface StepAPI {
  run<T>(name: string, fn: () => Promise<T> | T, opts?: StepRunOptions): Promise<T>;
  sleep(name: string, durationMs: number): Promise<void>;
  waitFor<T = unknown>(name: string, opts?: StepWaitOptions): Promise<T | null>;
}

/** Sentinel: end the run cleanly as `cancelled`. */
export class BailSignal {
  constructor(public reason?: string) {}
}

/** Sentinel: end the run as `dead` immediately (skip retries). */
export class FailSignal {
  constructor(public error: Error) {}
}

/** Sentinel: defer the run to `wakeAt` (or indefinitely if null). */
export class DeferSignal {
  constructor(public wakeAt: Date | null) {}
}

/** Sentinel: park the run waiting for an event. */
export class WaitSignal {
  constructor(
    public stepName: string,
    public eventName: string,
    public timeoutAt: Date | null,
  ) {}
}

/** True for any control-flow sentinel — these must propagate untouched. */
export function isControlSignal(err: unknown): boolean {
  return (
    err instanceof BailSignal ||
    err instanceof FailSignal ||
    err instanceof DeferSignal ||
    err instanceof WaitSignal
  );
}

export function createStepAPI(
  client: WorkflowsClient,
  runId: RunId,
  stepState: StepState,
): StepAPI {
  return {
    async run<T>(name: string, fn: () => Promise<T> | T, opts?: StepRunOptions): Promise<T> {
      if (Object.prototype.hasOwnProperty.call(stepState, name)) {
        return stepState[name] as T;
      }

      const attempts = (opts?.retries ?? 0) + 1;
      const base = opts?.backoff?.base ?? 1_000;
      const max = opts?.backoff?.max ?? 30_000;

      let lastErr: unknown;
      for (let attempt = 1; attempt <= attempts; attempt++) {
        try {
          const result = await fn();
          stepState[name] = result as unknown;
          await client.saveStep(runId, name, result ?? null);
          return result;
        } catch (err) {
          // Control signals (success/bail/fail/defer/wait) are how the
          // handler talks to the worker — never retry, never wrap, just
          // propagate.
          if (isControlSignal(err)) throw err;
          lastErr = err;
          if (opts?.retry === false) {
            const e = err instanceof Error ? err : new Error(String(err));
            throw new FailSignal(e);
          }
          if (attempt < attempts) {
            await new Promise((r) => setTimeout(r, expBackoffMs(attempt, base, max)));
          }
        }
      }
      throw lastErr;
    },

    async sleep(name: string, durationMs: number): Promise<void> {
      const key = `__sleep:${name}`;
      const stored = stepState[key] as { wakeAt: number } | undefined;
      if (stored) {
        if (Date.now() >= stored.wakeAt) {
          if (!Object.prototype.hasOwnProperty.call(stepState, name)) {
            stepState[name] = true;
            await client.saveStep(runId, name, true);
          }
          return;
        }
        throw new DeferSignal(new Date(stored.wakeAt));
      }

      const wakeAt = Date.now() + durationMs;
      stepState[key] = { wakeAt };
      await client.saveStep(runId, key, { wakeAt });

      if (durationMs < INLINE_SLEEP_THRESHOLD_MS) {
        await new Promise((r) => setTimeout(r, durationMs));
        stepState[name] = true;
        await client.saveStep(runId, name, true);
        return;
      }
      throw new DeferSignal(new Date(wakeAt));
    },

    async waitFor<T = unknown>(name: string, opts?: StepWaitOptions): Promise<T | null> {
      if (Object.prototype.hasOwnProperty.call(stepState, name)) {
        return stepState[name] as T | null;
      }
      const timeoutAt = opts?.timeout != null ? new Date(Date.now() + opts.timeout) : null;
      throw new WaitSignal(name, name, timeoutAt);
    },
  };
}
