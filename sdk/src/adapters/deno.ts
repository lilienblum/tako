/**
 * tako.sh Deno Adapter
 *
 * Provides Deno-specific Tako functionality.
 *
 * @example
 * ```typescript
 * import { Tako, serve } from 'tako.sh/deno';
 *
 * const tako = new Tako({
 *   onConfigReload: (secrets) => {
 *     console.log('Config reloaded:', secrets);
 *   }
 * });
 *
 * serve((request: Request) => {
 *   return new Response("Hello from Deno!");
 * });
 * ```
 */

import { Tako } from "../tako";
import type { TakoOptions, TakoStatus, FetchHandler } from "../types";
import { handleTakoEndpoint } from "../endpoints";

// Re-export core classes
export { Tako } from "../tako";
export type { TakoOptions, TakoStatus, FetchHandler } from "../types";

function resolveFetch(handler: FetchHandler) {
  if (typeof handler === "function") {
    return handler;
  }
  return handler.fetch.bind(handler);
}

// Environment variables set by tako (Deno uses Deno.env.get)
const getEnv = (key: string, defaultValue: string = ""): string => {
  try {
    // @ts-ignore - Deno global
    return Deno.env.get(key) ?? defaultValue;
  } catch {
    return defaultValue;
  }
};

const TAKO_VERSION = getEnv("TAKO_VERSION", "unknown");
const TAKO_INSTANCE = parseInt(getEnv("TAKO_INSTANCE", "1"), 10);

const startedAt = Date.now();
let status: TakoStatus["status"] = "starting";

/**
 * Get current Tako status
 */
export function getStatus(): TakoStatus {
  // @ts-ignore - Deno global
  const pid = typeof Deno !== "undefined" ? Deno.pid : 0;

  return {
    status,
    app: "app",
    version: TAKO_VERSION,
    instance_id: TAKO_INSTANCE,
    pid,
    uptime_seconds: Math.floor((Date.now() - startedAt) / 1000),
  };
}

/**
 * Create a Tako-wrapped Deno server
 *
 * This wraps Deno.serve() with Tako functionality including:
 * - Internal status endpoint on Host `tako.internal` + `/status`
 * - Graceful shutdown handling
 */
export function serve(
  handler: FetchHandler,
  options?: {
    port?: number;
    tako?: TakoOptions;
  },
): void {
  const port = options?.port ?? parseInt(getEnv("PORT", "3000"), 10);
  const userFetch = resolveFetch(handler);

  // Build environment object
  const env: Record<string, string> = {};
  try {
    // @ts-ignore - Deno global
    for (const [key, value] of Deno.env.toObject()) {
      env[key] = value;
    }
  } catch {
    // Environment access may be restricted
  }

  // Create fetch wrapper with Tako endpoints
  const wrappedFetch = async (request: Request): Promise<Response> => {
    // Check for Tako internal endpoints first
    const takoResponse = handleTakoEndpoint(request, getStatus());
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

  status = "healthy";

  // Start Deno server
  // @ts-ignore - Deno global
  Deno.serve({ port }, wrappedFetch);

  console.log(`[tako.sh] Deno server listening on http://localhost:${port}`);

  // Handle shutdown signals
  // @ts-ignore - Deno global
  if (typeof Deno !== "undefined") {
    // @ts-ignore - Deno global
    Deno.addSignalListener?.("SIGTERM", () => {
      console.log("[tako.sh] Received SIGTERM, draining...");
      status = "draining";
    });
  }
}
