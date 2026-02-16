/**
 * Tako Internal Endpoints
 *
 * These endpoints are handled by the SDK automatically.
 */

import type { TakoStatus } from "./types";

export const TAKO_INTERNAL_HOST = "tako.internal";
export const TAKO_INTERNAL_STATUS_PATH = "/status";

function normalizeHost(value: string | null): string | null {
  if (!value) {
    return null;
  }
  const normalized = value.trim().toLowerCase();
  if (normalized.length === 0) {
    return null;
  }
  return normalized.split(":")[0];
}

function requestHost(request: Request, url: URL): string | null {
  const hostFromHeader = normalizeHost(request.headers.get("host"));
  if (hostFromHeader) {
    return hostFromHeader;
  }
  return normalizeHost(url.host);
}

/**
 * Handle Tako internal endpoints (internal host only).
 */
export function handleTakoEndpoint(request: Request, status: TakoStatus): Response | null {
  const url = new URL(request.url);
  const host = requestHost(request, url);
  const path = url.pathname;

  if (host !== TAKO_INTERNAL_HOST) {
    return null;
  }

  switch (path) {
    case TAKO_INTERNAL_STATUS_PATH:
      return handleStatus(status);

    default:
      return new Response(JSON.stringify({ error: "Not found" }), {
        status: 404,
        headers: { "Content-Type": "application/json" },
      });
  }
}

/**
 * GET /status on tako.internal - Full status information
 */
function handleStatus(status: TakoStatus): Response {
  return new Response(JSON.stringify(status), {
    status: 200,
    headers: { "Content-Type": "application/json" },
  });
}
