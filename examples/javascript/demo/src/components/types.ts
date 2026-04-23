import type { MissionLogEvent, Step, StepState, StepStatus, WorkflowStep } from "../server/types";
import { EMPTY_RETRIES, EMPTY_STEPS, PIPELINE_STEPS } from "../server/types";

export type {
  MissionLogEvent,
  Step as PipelineStep,
  StepState as PipelineStepState,
  StepStatus,
  WorkflowStep,
};
export { EMPTY_RETRIES, EMPTY_STEPS, PIPELINE_STEPS };

export const PIPELINE_STEP_LABELS: Record<Step, string> = {
  check: "Check",
  pack: "Pack",
  load: "Load",
  ship: "Ship",
  deliver: "Deliver",
};

export type InFlightRequest = {
  requestId: string;
  base: string;
  item: string;
  createdAt: number;
  isComplete: boolean;
  steps: Record<Step, StepState>;
  retries: Record<Step, number>;
};

export function formatBaseName(slug: string): string {
  return slug
    .split(/[-_]/)
    .filter(Boolean)
    .map((word) => word.charAt(0).toUpperCase() + word.slice(1))
    .join(" ");
}

export function shortRequestId(requestId: string): string {
  return requestId.replace(/-/g, "").slice(0, 6).toUpperCase();
}

export function formatTimestamp(ms: number): string {
  const date = new Date(ms);
  const hh = date.getHours().toString().padStart(2, "0");
  const mm = date.getMinutes().toString().padStart(2, "0");
  const ss = date.getSeconds().toString().padStart(2, "0");
  return `${hh}:${mm}:${ss}`;
}

export function totalRetries(retries: Record<Step, number>): number {
  return PIPELINE_STEPS.reduce((sum, step) => sum + retries[step], 0);
}
