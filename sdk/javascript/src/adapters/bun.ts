/**
 * tako.sh Bun Adapter
 *
 * Provides Bun-specific Tako functionality.
 *
 * @example
 * ```typescript
 * import { Tako } from 'tako.sh/bun';
 *
 * export default function fetch(request: Request, env: Record<string, string>) {
 *   return new Response("Hello from Bun!");
 * }
 * ```
 */

import type { TakoOptions, TakoStatus, FetchHandler } from "../types";
import { handleTakoEndpoint } from "../endpoints";

// Re-export core classes
export { Tako } from "../tako";
export type { TakoOptions, TakoStatus, FetchHandler } from "../types";

/**
 * Create a Tako-wrapped Bun server
 *
 * This wraps Bun.serve() with Tako functionality including:
 * - Internal status endpoint on Host `tako` + `/status`
 * - Graceful shutdown handling
 */
export function serve(
  handler: FetchHandler,
  options?: {
    host?: string;
    port?: number;
    tako?: TakoOptions;
  },
): void {
  const host = options?.host ?? process.env["HOST"] ?? "127.0.0.1";
  const port = options?.port ?? parseInt(process.env["PORT"] || "3000", 10);
  const userFetch = handler;

  // Environment variables set by tako
  const TAKO_VERSION = process.env["TAKO_VERSION"] || "unknown";
  const TAKO_INSTANCE = process.env["TAKO_INSTANCE"] || "unknown";

  const startedAt = Date.now();
  let status: TakoStatus["status"] = "starting";

  const getStatus = (): TakoStatus => ({
    status,
    app: "app",
    version: TAKO_VERSION,
    instance_id: TAKO_INSTANCE,
    pid: process.pid,
    uptime_seconds: Math.floor((Date.now() - startedAt) / 1000),
  });

  // Build environment object
  const env: Record<string, string> = {};
  for (const [key, value] of Object.entries(process.env)) {
    if (value !== undefined) {
      env[key] = value;
    }
  }

  // Create fetch wrapper with Tako endpoints
  const wrappedFetch = async (request: Request): Promise<Response> => {
    // Check for Tako internal endpoints first
    const takoResponse = await handleTakoEndpoint(request, getStatus());
    if (takoResponse) {
      return takoResponse;
    }

    // Pass through to user handler
    try {
      return await userFetch(request, env);
    } catch (err) {
      console.error("[tako.sh] Error in fetch handler:", err);
      return new Response(JSON.stringify({ error: "Internal Server Error" }), {
        status: 500,
        headers: { "Content-Type": "application/json" },
      });
    }
  };

  Bun.serve({
    hostname: host,
    port,
    fetch: wrappedFetch,
  });
  console.log(`[tako.sh] Bun server listening on http://${host}:${port}`);

  status = "healthy";

  // Handle shutdown
  process.on("SIGTERM", () => {
    status = "draining";
  });
}
