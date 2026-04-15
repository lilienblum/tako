/**
 * Shared types for Tako's durable workflow engine.
 *
 * Vocabulary:
 *   workflow — a named handler (the thing you write in `workflows/*.ts`)
 *   run      — one execution of a workflow (the row in the queue)
 *   step     — a memoized portion inside a run (via `ctx.step.run`)
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
  /** Max attempts (inclusive of the first try). Defaults to 3. */
  maxAttempts?: number;
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
  maxAttempts: number;
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
  /** Run-level retry budget. Default 3. */
  maxAttempts?: number;
  /** Run-level backoff between failed attempts. */
  backoff?: { base?: number; max?: number };
  /** Worker concurrency per instance. Default 10. */
  concurrency?: number;
  /** Handler timeout in ms. Default unbounded. */
  timeoutMs?: number;
  /** Cron expression — if present, the workflow is scheduled. */
  schedule?: string;
}
