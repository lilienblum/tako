/**
 * tako.sh Deno Adapter (internal)
 *
 * Used by the tako-deno entrypoint binary. Not a public API.
 */

import type { TakoOptions, TakoStatus, FetchHandler } from "../types";
import { handleTakoEndpoint } from "../endpoints";

// Re-export core classes
export { Tako } from "../tako";
export type { TakoOptions, TakoStatus, FetchHandler } from "../types";

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
const TAKO_INSTANCE = getEnv("TAKO_INSTANCE", "unknown");

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
  const host = options?.host ?? getEnv("HOST", "127.0.0.1");
  const port = options?.port ?? parseInt(getEnv("PORT", "3000"), 10);
  const userFetch = handler;

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

  status = "healthy";

  // Start Deno server
  // @ts-ignore - Deno global
  Deno.serve({ hostname: host, port }, wrappedFetch);

  console.log(`[tako.sh] Deno server listening on http://${host}:${port}`);

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
