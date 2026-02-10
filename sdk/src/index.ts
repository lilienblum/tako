/**
 * tako.sh - Official SDK for Tako development, deployment, and runtime platform
 *
 * Runtime SDK for Tako apps. Provides optional features for apps deployed with Tako.
 *
 * @example
 * ```typescript
 * // Basic usage - no SDK needed!
 * export default {
 *   fetch(request: Request, env: Record<string, string>) {
 *     return new Response("Hello World!");
 *   }
 * };
 * ```
 *
 * @example
 * ```typescript
 * // With SDK features (auto-detect runtime)
 * import { Tako } from 'tako.sh';
 *
 * const tako = new Tako({
 *   onConfigReload: (secrets) => {
 *     // Handle secret updates at runtime
 *     database.reconnect(secrets.DATABASE_URL);
 *   }
 * });
 *
 * export default {
 *   fetch(request: Request, env: Record<string, string>) {
 *     return new Response("Hello World!");
 *   }
 * };
 * ```
 *
 * @example
 * ```typescript
 * // Runtime-specific imports
 * import { Tako } from 'tako.sh/bun';   // Bun
 * import { Tako } from 'tako.sh/node';  // Node.js
 * import { Tako } from 'tako.sh/deno';  // Deno
 * ```
 *
 * @packageDocumentation
 */

export { Tako } from "./tako";
export type {
  FetchHandler,
  TakoOptions,
  TakoStatus,
  AppToServerMessage,
  ServerToAppMessage,
  ReadyMessage,
  HeartbeatMessage,
  ShutdownAckMessage,
  ShutdownMessage,
  ReloadConfigMessage,
  ServerAck,
} from "./types";
