/**
 * Server-side Tako endpoint handler for framework integrations.
 *
 * Use this in framework adapters (Vite plugins, Next.js middleware, etc.)
 * to handle Tako internal protocol requests (channel auth, health checks).
 *
 * @example
 * ```ts
 * import { handleTakoEndpoint } from "tako.sh/server";
 *
 * export default async function handler(request: Request) {
 *   const takoResponse = await handleTakoEndpoint(request, getStatus());
 *   if (takoResponse) return takoResponse;
 *   return yourAppHandler(request);
 * }
 * ```
 */

export { handleTakoEndpoint } from "./tako/endpoints";
export type { TakoStatus } from "./types";
