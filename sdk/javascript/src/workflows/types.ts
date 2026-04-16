/**
 * Shared types for Tako's durable workflow engine.
 *
 * Vocabulary:
 *   workflow — a named handler (the thing you write in `workflows/*.ts`)
 *   run      — one execution of a workflow (the row in the queue)
 *   step     — a memoized portion inside a run (via `ctx.run`)
 */

export type RunId = string;

export type RunStatus = "pending" | "running" | "succeeded" | "cancelled" | "dead";

export type StepState = Record<string, unknown>;

export interface RunSpec {
  /** Workflow name — must match a registered handler. */
  name: string;
  /** JSON-serializable user payload. */
  payload: unknown;
  /** When to run. Defaults to now. */
  runAt?: Date;
  /** Number of retries after the first attempt. */
  retries?: number;
  /**
   * Uniqueness key. If a run with this key already exists in a
   * non-terminal state, enqueue is a no-op and the existing run id is
   * returned. Used by cron to avoid duplicate ticks across replicas.
   */
  uniqueKey?: string | null;
}

export interface Run {
  id: RunId;
  name: string;
  payload: unknown;
  status: RunStatus;
  attempts: number;
  retries: number;
  /** Unix ms. */
  runAt: number;
  /** Unix ms; null for non-running runs. */
  leaseUntil: number | null;
  workerId: string | null;
  lastError: string | null;
  stepState: StepState;
  /** Unix ms. */
  createdAt: number;
  uniqueKey: string | null;
}

export interface WorkflowConfig {
  /** Number of retries after the first attempt. Default 2 (3 total attempts). */
  retries?: number;
  /** Run-level backoff between failed attempts. */
  backoff?: { base?: number; max?: number };
  /** Worker concurrency per instance. Default 10. */
  concurrency?: number;
  /** Handler timeout in ms. Default unbounded. */
  timeoutMs?: number;
  /**
   * Cron expression (5-field: minute hour day-of-month month day-of-week).
   * @example "0 9 * * 1-5"    — weekdays at 9am
   * @example "&#42;/15 * * * *" — every 15 minutes
   */
  schedule?: string;
}
