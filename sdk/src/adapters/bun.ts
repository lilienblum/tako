/**
 * tako.sh Bun Adapter
 *
 * Provides Bun-specific Tako functionality.
 *
 * @example
 * ```typescript
 * import { Tako } from 'tako.sh/bun';
 *
 * const tako = new Tako({
 *   onConfigReload: (secrets) => {
 *     console.log('Config reloaded:', secrets);
 *   }
 * });
 *
 * export default function fetch(request: Request, env: Record<string, string>) {
 *   return new Response("Hello from Bun!");
 * }
 * ```
 */

import { Tako } from "../tako";
import type { TakoOptions, TakoStatus, FetchHandler } from "../types";
import { ServerConnection } from "../connection";
import { handleTakoEndpoint } from "../endpoints";
import { resolveAppSocketPath } from "../socket-path";

// Re-export core classes
export { Tako } from "../tako";
export type { TakoOptions, TakoStatus, FetchHandler } from "../types";

function resolveFetch(handler: FetchHandler) {
  if (typeof handler === "function") {
    return handler;
  }
  return handler.fetch.bind(handler);
}

/**
 * Create a Tako-wrapped Bun server
 *
 * This wraps Bun.serve() with Tako functionality including:
 * - Internal status endpoint on Host `tako.internal` + `/status`
 * - Automatic heartbeat to tako-server
 * - Graceful shutdown handling
 */
export function serve(
  handler: FetchHandler,
  options?: {
    port?: number;
    tako?: TakoOptions;
  },
): void {
  const port = options?.port ?? parseInt(process.env.PORT || "3000", 10);
  const takoOptions = options?.tako ?? {};
  const userFetch = resolveFetch(handler);

  // Environment variables set by tako
  const TAKO_SOCKET = process.env.TAKO_SOCKET;
  const TAKO_VERSION = process.env.TAKO_VERSION || "unknown";
  const TAKO_INSTANCE = parseInt(process.env.TAKO_INSTANCE || "1", 10);
  const TAKO_APP_SOCKET = process.env.TAKO_APP_SOCKET;
  const appSocketPath = resolveAppSocketPath(TAKO_APP_SOCKET);

  const DEFAULT_TAKO_SOCKET = "/var/run/tako/tako.sock";
  const serverSocketPath = TAKO_SOCKET || DEFAULT_TAKO_SOCKET;

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

  // Start server
  if (appSocketPath) {
    // Production: Unix socket
    Bun.serve({
      unix: appSocketPath,
      fetch: wrappedFetch,
    });
    console.log(`[tako.sh] Bun server listening on ${appSocketPath}`);
  } else {
    // Development: TCP
    Bun.serve({
      port,
      fetch: wrappedFetch,
    });
    console.log(`[tako.sh] Bun server listening on http://localhost:${port}`);
  }

  status = "healthy";

  // Connect to tako-server in production
  if (appSocketPath) {
    const connection = new ServerConnection(
      serverSocketPath,
      "app",
      TAKO_VERSION,
      TAKO_INSTANCE,
      appSocketPath,
      takoOptions,
    );

    connection
      .connect()
      .then(() => {
        connection.startHeartbeat();
      })
      .catch((err) => {
        console.error("[tako.sh] Failed to connect to tako-server:", err);
      });

    // Handle shutdown
    process.on("SIGTERM", () => {
      status = "draining";
    });
  }
}
