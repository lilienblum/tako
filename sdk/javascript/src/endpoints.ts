/**
 * Tako Internal Endpoints
 *
 * These endpoints are handled by the SDK automatically on Host: tako.
 *
 * - GET  /status — Health/status check
 */

import type { TakoStatus } from "./types";

export const TAKO_INTERNAL_HOST = "tako";
export const TAKO_INTERNAL_STATUS_PATH = "/status";
export const TAKO_INTERNAL_TOKEN_ENV = "TAKO_INTERNAL_TOKEN";
export const TAKO_INTERNAL_TOKEN_HEADER = "x-tako-internal-token";
const LOOPBACK_INTERNAL_HOSTS = new Set(["127.0.0.1", "localhost", "0.0.0.0"]);

function normalizeHost(value: string | null): string | null {
  if (!value) {
    return null;
  }
  const normalized = value.trim().toLowerCase();
  if (normalized.length === 0) {
    return null;
  }
  const [host = ""] = normalized.split(":");
  return host;
}

function isInternalHost(host: string | null): boolean {
  if (!host) {
    return false;
  }
  return host === TAKO_INTERNAL_HOST || LOOPBACK_INTERNAL_HOSTS.has(host);
}

function internalToken(): string | null {
  if (typeof process !== "undefined") {
    const token = process.env?.[TAKO_INTERNAL_TOKEN_ENV];
    if (token) {
      return token;
    }
  }

  const maybeDeno = (
    globalThis as { Deno?: { env?: { get: (key: string) => string | undefined } } }
  ).Deno;
  if (maybeDeno?.env) {
    try {
      const token = maybeDeno.env.get(TAKO_INTERNAL_TOKEN_ENV);
      if (token) {
        return token;
      }
    } catch {
      // ignore env access failures
    }
  }

  return null;
}

function internalResponse(
  body: unknown,
  status: number,
  token: string,
  extraHeaders?: Record<string, string>,
): Response {
  return new Response(JSON.stringify(body), {
    status,
    headers: {
      "Content-Type": "application/json",
      [TAKO_INTERNAL_TOKEN_HEADER]: token,
      ...extraHeaders,
    },
  });
}

/**
 * Handle Tako internal endpoints (internal host only).
 *
 * Returns a Response for internal requests, or null for non-internal requests.
 */
export function handleTakoEndpoint(request: Request, status: TakoStatus): Response | null {
  // Fast path: check Host header before parsing the URL (avoids allocation for normal traffic)
  const hostHeader = normalizeHost(request.headers.get("host"));
  if (hostHeader && !isInternalHost(hostHeader)) {
    return null;
  }

  const url = new URL(request.url);
  const host = hostHeader || normalizeHost(url.host);
  if (!isInternalHost(host)) {
    return null;
  }

  const token = internalToken();
  const path = url.pathname;
  if (!token || request.headers.get(TAKO_INTERNAL_TOKEN_HEADER) !== token) {
    return new Response(JSON.stringify({ error: "Forbidden" }), {
      status: 403,
      headers: { "Content-Type": "application/json" },
    });
  }

  switch (path) {
    case TAKO_INTERNAL_STATUS_PATH:
      return handleStatus(status, token);

    default:
      return internalResponse({ error: "Not found" }, 404, token);
  }
}

/**
 * GET /status on Host: tako — Full status information
 */
function handleStatus(status: TakoStatus, token: string): Response {
  return internalResponse(status, 200, token);
}
