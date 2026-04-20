/**
 * tako.sh - Official SDK for Tako development, deployment, and runtime platform
 *
 * @example
 * ```typescript
 * export default function fetch(request: Request, env: Record<string, string>) {
 *   return new Response("Hello World!");
 * }
 * ```
 *
 * @packageDocumentation
 */

import { setChannelSocketPublisher } from "./channels";
import type { ChannelMessage } from "./types";
import {
  assertInternalSocketEnvConsistency,
  callInternal,
  internalSocketFromEnv,
} from "./internal-socket";

// Fail loud at import time if the Tako runtime env contract is half-set
// (e.g. TAKO_APP_NAME present but TAKO_INTERNAL_SOCKET missing). This turns
// what used to be a runtime-only error on the first `enqueue` / `publish`
// into a boot-time crash, so misconfigured spawns surface in server logs
// immediately instead of hiding until a user clicks a button.
assertInternalSocketEnvConsistency();

// Install the server-side publisher so `new Channel("x").publish(...)`
// from app/workflow code goes over the Tako internal unix socket
// instead of round-tripping through HTTPS + auth.
setChannelSocketPublisher(async <T>(channel: string, message: unknown) => {
  const internal = internalSocketFromEnv();
  if (!internal) {
    throw new Error(
      "Tako.channels.publish called outside a Tako-managed process (TAKO_INTERNAL_SOCKET + TAKO_APP_NAME not set).",
    );
  }
  const data = await callInternal(internal.socketPath, {
    command: "channel_publish",
    app: internal.app,
    channel,
    payload: message,
  });
  return data as ChannelMessage<T>;
});

export { Tako } from "./tako";
export { Channel } from "./channels";
export { defineChannel } from "./channels/define";
export { defineWorkflow } from "./workflows";
export { TakoError, type TakoErrorCode } from "./internal-socket";

/**
 * Extract the payload type from a workflow definition.
 * Used by the generated `tako.d.ts` to type `Tako.workflows.enqueue`.
 *
 * @example
 * ```ts
 * type P = InferWorkflowPayload<typeof import("./workflows/send-email").default>;
 * ```
 */
export type InferWorkflowPayload<T> = T extends import("./workflows").WorkflowDefinition<infer P>
  ? P
  : unknown;
