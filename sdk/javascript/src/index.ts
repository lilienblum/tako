/**
 * tako.sh - Official SDK for Tako development, deployment, and runtime platform
 *
 * Runtime SDK for Tako apps. Provides optional features for apps deployed with Tako.
 *
 * @example
 * ```typescript
 * // Basic usage - no SDK needed!
 * export default function fetch(request: Request, env: Record<string, string>) {
 *   return new Response("Hello World!");
 * }
 * ```
 *
 * @packageDocumentation
 */

export { Tako, installTakoGlobal } from "./tako";
export { Channel, ChannelRegistry, TAKO_CHANNELS_BASE_PATH } from "./channels";
export type {
  EnqueueOptions,
  EnqueueResult,
  Run,
  RunId,
  RunSpec,
  RunStatus,
  StepAPI,
  StepState,
  WorkflowConfig,
  WorkflowContext,
  WorkflowHandler,
} from "./workflows";
export { WorkflowsClient, WorkflowsError } from "./workflows";
export type {
  ChannelAuthContext,
  ChannelAuthorizeInput,
  ChannelAuthorizeResponse,
  ChannelConnectOptions,
  ChannelDefinition,
  ChannelDefinitionTransport,
  ChannelGrant,
  ChannelMessage,
  ChannelOperation,
  ChannelConnection,
  ChannelPublishInput,
  ChannelPublishOptions,
  ChannelSubscribeOptions,
  ChannelSubscription,
  FetchHandler,
  TakoStatus,
} from "./types";
