/**
 * Tako Internal Endpoints
 *
 * These endpoints are handled by the SDK automatically on Host: tako.
 *
 * - GET  /status       — Health/status check
 * - POST /secrets  — Receive secrets from tako-server
 */

import type { TakoStatus } from "./types";
import { injectSecrets } from "./secrets";

export const TAKO_INTERNAL_HOST = "tako";
export const TAKO_INTERNAL_STATUS_PATH = "/status";
export const TAKO_INTERNAL_SECRETS_PATH = "/secrets";

function normalizeHost(value: string | null): string | null {
  if (!value) {
    return null;
  }
  const normalized = value.trim().toLowerCase();
  if (normalized.length === 0) {
    return null;
  }
  return normalized.split(":")[0] ?? null;
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
 *
 * Returns a Response for internal requests, or null for non-internal requests.
 * Async because POST /secrets needs to read the request body.
 */
export async function handleTakoEndpoint(
  request: Request,
  status: TakoStatus,
): Promise<Response | null> {
  const url = new URL(request.url);
  const host = requestHost(request, url);
  const path = url.pathname;

  if (host !== TAKO_INTERNAL_HOST) {
    return null;
  }

  switch (path) {
    case TAKO_INTERNAL_STATUS_PATH:
      return handleStatus(status);

    case TAKO_INTERNAL_SECRETS_PATH:
      return handleSetSecrets(request);

    default:
      return new Response(JSON.stringify({ error: "Not found" }), {
        status: 404,
        headers: { "Content-Type": "application/json" },
      });
  }
}

/**
 * GET /status on Host: tako — Full status information
 */
function handleStatus(status: TakoStatus): Response {
  return new Response(JSON.stringify(status), {
    status: 200,
    headers: { "Content-Type": "application/json" },
  });
}

/**
 * POST /secrets on Host: tako — Receive secrets from tako-server
 */
async function handleSetSecrets(request: Request): Promise<Response> {
  if (request.method !== "POST") {
    return new Response(JSON.stringify({ error: "Method not allowed" }), {
      status: 405,
      headers: { "Content-Type": "application/json" },
    });
  }

  try {
    const secrets = await request.json();
    injectSecrets(secrets);
    return new Response(JSON.stringify({ status: "ok" }), {
      status: 200,
      headers: { "Content-Type": "application/json" },
    });
  } catch {
    return new Response(JSON.stringify({ error: "Invalid JSON body" }), {
      status: 400,
      headers: { "Content-Type": "application/json" },
    });
  }
}
