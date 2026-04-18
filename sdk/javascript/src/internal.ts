/**
 * tako.sh/internal — Types and utilities for advanced use cases.
 *
 * Most apps only need `tako.sh`. Import from here when you need to
 * type workflow handlers, channel auth, or build framework adapters.
 */

// Workflow authoring types
export type {
  WorkflowDefinition,
  WorkflowHandler,
  WorkflowContext,
  WorkflowConfig,
} from "./workflows";
export type { EnqueueOptions, Workflows } from "./workflows";
export type { StepRunOptions, StepWaitOptions } from "./workflows";
export { isWorkflowDefinition } from "./workflows";

// Channel types
export type {
  ChannelConnectOptions,
  ChannelDefinitionTransport,
  ChannelGrant,
  ChannelMessage,
  ChannelPublishInput,
  ChannelPublishOptions,
  ChannelSocket,
  ChannelSubscribeOptions,
  ChannelSubscription,
  FetchHandler,
} from "./types";
export type { ChannelDefinition, ChannelAuthContext } from "./channels/define";
export { defineChannel, isChannelDefinition } from "./channels/define";

// Re-exports used by entrypoints and framework adapters
export { ChannelRegistry, TAKO_CHANNELS_BASE_PATH } from "./channels";
export { installTakoGlobal } from "./tako";
