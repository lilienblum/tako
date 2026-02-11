/**
 * Tako Internal Endpoints
 *
 * These endpoints are handled by the SDK automatically.
 */

import type { TakoStatus } from "./types";

/**
 * Handle Tako internal endpoints (/_tako/*)
 */
export function handleTakoEndpoint(request: Request, status: TakoStatus): Response | null {
  const url = new URL(request.url);
  const path = url.pathname;

  if (!path.startsWith("/_tako/")) {
    return null;
  }

  switch (path) {
    case "/_tako/status":
      return handleStatus(status);

    case "/_tako/health":
      return handleHealth(status);

    default:
      return new Response(JSON.stringify({ error: "Not found" }), {
        status: 404,
        headers: { "Content-Type": "application/json" },
      });
  }
}

/**
 * GET /_tako/status - Full status information
 */
function handleStatus(status: TakoStatus): Response {
  return new Response(JSON.stringify(status), {
    status: 200,
    headers: { "Content-Type": "application/json" },
  });
}

/**
 * GET /_tako/health - Simple health check
 */
function handleHealth(status: TakoStatus): Response {
  const isHealthy = status.status === "healthy";

  return new Response(
    JSON.stringify({
      status: isHealthy ? "ok" : status.status,
    }),
    {
      status: isHealthy ? 200 : 503,
      headers: { "Content-Type": "application/json" },
    },
  );
}
