import type { WorkflowConfig } from "./types";
import type { WorkflowHandler } from "./worker";

export const WORKFLOW_SYMBOL = Symbol("workflow");

export interface WorkflowDefinition<P = unknown> {
  readonly type: typeof WORKFLOW_SYMBOL;
  readonly handler: WorkflowHandler<P>;
  readonly config: WorkflowConfig;
}

/**
 * Defines a workflow with optional configuration. Returns a
 * `WorkflowDefinition` object that the file-based discovery system reads
 * to register the handler and its config (retries, schedule, etc.).
 *
 * @example
 * ```ts
 * import { defineWorkflow } from "tako.sh";
 *
 * export default defineWorkflow(async (ctx, payload: { userId: string }) => {
 *   await ctx.step.run("send", () => sendEmail(payload.userId));
 * }, { maxAttempts: 4, schedule: "0 9 * * *" });
 * ```
 */
export function defineWorkflow<P = unknown>(
  handler: WorkflowHandler<P>,
  config: WorkflowConfig = {},
): WorkflowDefinition<P> {
  return { type: WORKFLOW_SYMBOL, handler, config };
}

/**
 * Type guard — returns true if `value` is a `WorkflowDefinition` produced
 * by `defineWorkflow`.
 */
export function isWorkflowDefinition(value: unknown): value is WorkflowDefinition {
  return (
    typeof value === "object" &&
    value !== null &&
    "type" in value &&
    "handler" in value &&
    (value as { type: unknown }).type === WORKFLOW_SYMBOL
  );
}
