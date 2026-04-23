export type WorkflowStep = "check" | "pack" | "load" | "ship" | "deliver" | "complete";

export type StepStatus = "running" | "done" | "failed";

export type MissionLogEvent = {
  id: string;
  requestId: string;
  timestamp: number;
  source: string;
  level: "info" | "warn" | "error";
  message: string;
  step?: WorkflowStep;
  status?: StepStatus;
};

export const PIPELINE_STEPS = ["check", "pack", "load", "ship", "deliver"] as const;
export type Step = (typeof PIPELINE_STEPS)[number];
export type StepState = "pending" | "running" | "done" | "failed";

export type DbSupplyRequest = {
  requestId: string;
  baseSlug: string;
  item: string;
  isComplete: boolean;
  steps: Record<Step, StepState>;
  retries: Record<Step, number>;
  createdAt: number;
  updatedAt: number;
};

export type MissionChannelUpdate = {
  request: DbSupplyRequest;
  event?: MissionLogEvent;
};

export type DbBase = {
  slug: string;
  createdAt: number;
};

export type BaseSnapshot = {
  base: DbBase;
  requests: DbSupplyRequest[];
};

export const EMPTY_STEPS: Record<Step, StepState> = {
  check: "pending",
  pack: "pending",
  load: "pending",
  ship: "pending",
  deliver: "pending",
};

export const EMPTY_RETRIES: Record<Step, number> = {
  check: 0,
  pack: 0,
  load: 0,
  ship: 0,
  deliver: 0,
};
