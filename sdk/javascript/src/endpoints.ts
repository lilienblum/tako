/**
 * Tako Internal Endpoints
 *
 * These endpoints are handled by the SDK automatically on Host: tako.internal.
 *
 * - GET  /status — Health/status check
 * - POST /channels/authorize — Channel auth callback
 */

import { Tako } from "./tako";
import type { ChannelAuthorizeInput, TakoStatus } from "./types";

export const TAKO_INTERNAL_HOST = "tako.internal";
export const TAKO_INTERNAL_STATUS_PATH = "/status";
export const TAKO_INTERNAL_CHANNELS_AUTHORIZE_PATH = "/channels/authorize";
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
export async function handleTakoEndpoint(
  request: Request,
  status: TakoStatus,
): Promise<Response | null> {
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
    case TAKO_INTERNAL_CHANNELS_AUTHORIZE_PATH:
      return await handleChannelAuthorize(request, token);

    default:
      return internalResponse({ error: "Not found" }, 404, token);
  }
}

/**
 * GET /status on Host: tako.internal — Full status information
 */
function handleStatus(status: TakoStatus, token: string): Response {
  return internalResponse(status, 200, token);
}

async function handleChannelAuthorize(request: Request, token: string): Promise<Response> {
  if (request.method !== "POST") {
    return internalResponse({ error: "Method not allowed" }, 405, token);
  }

  let input: ChannelAuthorizeInput;
  try {
    input = (await request.json()) as ChannelAuthorizeInput;
  } catch {
    return internalResponse({ error: "Invalid JSON", ok: false }, 400, token);
  }

  if (!input.channel || !input.operation || !input.request?.url) {
    return internalResponse({ error: "Invalid request", ok: false }, 400, token);
  }

  const result = await Tako.channels.authorize(input);
  if (!result.ok) {
    const hasDefinition = Tako.channels.resolveDefinition(input.channel) !== null;
    if (!hasDefinition) {
      return internalResponse({ error: "Channel not defined", ok: false }, 404, token);
    }
    return internalResponse({ error: "Forbidden", ok: false }, 403, token);
  }

  return internalResponse(result, 200, token);
}
